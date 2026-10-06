package config

import (
	"errors"
	"reflect"
	"strings"
	"testing"
)

// TestLoadValidHatch round-trips a TOML document containing only the
// allow-listed fields. Every value must reach the struct byte-for-byte;
// no forbidden-field error.
func TestLoadValidHatch(t *testing.T) {
	const doc = `
data_dir = "/var/lib/fauna-mail-bridge-test"
metrics_bind_addr = "127.0.0.1:19090"
mta_bind_addr = "127.0.0.1:12525"
imap_listen_implicit_tls = "[::]:9993"
imap_listen_starttls = "[::]:9143"
caldav_listen_https = "[::]:9443"
caldav_bind_host = "192.168.1.57"

[mta_mx_override]
"external.test" = "127.0.0.1:2526"
"internal.corp" = "relay.corp"
`
	hatch, err := LoadFromReader(strings.NewReader(doc))
	if err != nil {
		t.Fatalf("LoadFromReader: %v", err)
	}
	if hatch == nil {
		t.Fatal("LoadFromReader returned nil hatch with nil error")
	}
	if got, want := hatch.DataDir, "/var/lib/fauna-mail-bridge-test"; got != want {
		t.Errorf("DataDir = %q, want %q", got, want)
	}
	if got, want := hatch.MetricsBindAddr, "127.0.0.1:19090"; got != want {
		t.Errorf("MetricsBindAddr = %q, want %q", got, want)
	}
	if got, want := hatch.MTABindAddr, "127.0.0.1:12525"; got != want {
		t.Errorf("MTABindAddr = %q, want %q", got, want)
	}
	if got, want := hatch.IMAPListenImplicitTLS, "[::]:9993"; got != want {
		t.Errorf("IMAPListenImplicitTLS = %q, want %q", got, want)
	}
	if got, want := hatch.IMAPListenStartTLS, "[::]:9143"; got != want {
		t.Errorf("IMAPListenStartTLS = %q, want %q", got, want)
	}
	if got, want := hatch.CalDAVListenHTTPS, "[::]:9443"; got != want {
		t.Errorf("CalDAVListenHTTPS = %q, want %q", got, want)
	}
	if got, want := hatch.CalDAVBindHost, "192.168.1.57"; got != want {
		t.Errorf("CalDAVBindHost = %q, want %q", got, want)
	}
	if got, want := hatch.MTAMXOverride["external.test"], "127.0.0.1:2526"; got != want {
		t.Errorf("MTAMXOverride[external.test] = %q, want %q", got, want)
	}
	if got, want := hatch.MTAMXOverride["internal.corp"], "relay.corp"; got != want {
		t.Errorf("MTAMXOverride[internal.corp] = %q, want %q", got, want)
	}
}

// TestRejectsImapPolicyKnobs guards that Tier-2 IMAP-policy knobs
// (`idle_timeout_secs`, `tombstone_retention_days`, `delete_nonempty`,
// `bodystructure_cache_max`) are NOT operator-hatch fields. They live
// in nest state per Phase A's ImapPolicy plumbing and reach the bridge
// via wsrpc.FetchConfig. An operator who pins one in the operator-hatch
// gets an actionable error pointing them at the Fauna admin UI.
func TestRejectsImapPolicyKnobs(t *testing.T) {
	for _, key := range []string{
		"idle_timeout_secs",
		"tombstone_retention_days",
		"delete_nonempty",
		"bodystructure_cache_max",
	} {
		t.Run(key, func(t *testing.T) {
			doc := key + ` = 300`
			if key == "delete_nonempty" {
				doc = key + ` = "allowed"`
			}
			assertForbiddenField(t, doc, key)
		})
	}
}

// TestRejectsLogDestination — log_destination was a speculative
// operator-hatch field that B.8 never honored. Removed at B.9 review;
// this test guards that any operator who set it gets a clear error
// (rather than silently-ignored behavior, which the B.9 review
// flagged as a Phase C debugging trap).
func TestRejectsLogDestination(t *testing.T) {
	_, err := LoadFromReader(strings.NewReader(`log_destination = "syslog:mail"`))
	var ffe *ErrForbiddenField
	if !errors.As(err, &ffe) {
		t.Fatalf("want ErrForbiddenField, got %v", err)
	}
	if ffe.Key != "log_destination" {
		t.Errorf("Key = %q, want log_destination", ffe.Key)
	}
}

// TestLoadEmpty asserts an empty TOML document is valid and yields a
// zero-valued *OperatorHatch (no overrides — the bridge falls back to
// compiled-in defaults for every field).
func TestLoadEmpty(t *testing.T) {
	hatch, err := LoadFromReader(strings.NewReader(""))
	if err != nil {
		t.Fatalf("LoadFromReader: %v", err)
	}
	if hatch == nil {
		t.Fatal("LoadFromReader returned nil hatch with nil error for empty input")
	}
	// reflect.DeepEqual (not ==): OperatorHatch carries a map field
	// (MTAMXOverride) and is no longer comparable. An empty doc yields a
	// zero-valued struct with a nil map.
	if !reflect.DeepEqual(*hatch, OperatorHatch{}) {
		t.Errorf("empty doc yielded non-zero hatch: %+v", *hatch)
	}
}

// TestLoadEmptyPath asserts Load("") returns (nil, nil) cleanly — the
// no-operator-hatch case is normal for a fresh deployment and must not
// be an error.
func TestLoadEmptyPath(t *testing.T) {
	hatch, err := Load("")
	if err != nil {
		t.Fatalf("Load(\"\"): %v", err)
	}
	if hatch != nil {
		t.Errorf("Load(\"\") = %+v, want nil", hatch)
	}
}

// assertForbiddenField is the common assertion for the per-tier
// rejection tests: returns a typed *ErrForbiddenField whose Key matches
// wantKey and whose Hint points at the Fauna admin UI.
func assertForbiddenField(t *testing.T, doc, wantKey string) {
	t.Helper()
	hatch, err := LoadFromReader(strings.NewReader(doc))
	if err == nil {
		t.Fatalf("LoadFromReader(%q): expected ErrForbiddenField, got nil error (hatch=%+v)", doc, hatch)
	}
	if hatch != nil {
		t.Errorf("LoadFromReader returned non-nil hatch alongside error: %+v", hatch)
	}
	var ffe *ErrForbiddenField
	if !errors.As(err, &ffe) {
		t.Fatalf("error type = %T (%v), want *ErrForbiddenField", err, err)
	}
	if ffe.Key != wantKey {
		t.Errorf("ErrForbiddenField.Key = %q, want %q", ffe.Key, wantKey)
	}
	if ffe.Hint == "" {
		t.Error("ErrForbiddenField.Hint is empty; must point operators at the Fauna admin UI")
	}
	if !strings.Contains(strings.ToLower(ffe.Hint), "fauna") || !strings.Contains(strings.ToLower(ffe.Hint), "app") {
		t.Errorf("ErrForbiddenField.Hint = %q; must mention the Fauna admin UI / app", ffe.Hint)
	}
	if !strings.Contains(err.Error(), wantKey) {
		t.Errorf("error message = %q; should mention the offending key %q", err.Error(), wantKey)
	}
}

// TestRejectsDKIMField asserts that a DKIM selector (Tier-2 admin
// surface) at the operator-hatch returns ErrForbiddenField. DKIM keys
// are provisioned via the admin pane in the Fauna app; pinning one
// in the operator-hatch would violate the product invariant.
func TestRejectsDKIMField(t *testing.T) {
	assertForbiddenField(t,
		`dkim_selector = "fauna1"`,
		"dkim_selector",
	)
}

// TestRejectsSpamField asserts that a spam threshold (Tier-2 admin
// policy) at the operator-hatch returns ErrForbiddenField.
func TestRejectsSpamField(t *testing.T) {
	assertForbiddenField(t,
		`spam_threshold = 7`,
		"spam_threshold",
	)
}

// TestRejectsPerAccountField asserts that a [accounts.<user>] table
// (Tier-3 per-user surface) at the operator-hatch returns
// ErrForbiddenField. Per-account opt-in is set by each user from their
// own Fauna app, never from a config file an operator hand-edits.
func TestRejectsPerAccountField(t *testing.T) {
	const doc = `
[accounts.alice]
opt_in = true
`
	hatch, err := LoadFromReader(strings.NewReader(doc))
	if err == nil {
		t.Fatalf("expected ErrForbiddenField, got nil error (hatch=%+v)", hatch)
	}
	var ffe *ErrForbiddenField
	if !errors.As(err, &ffe) {
		t.Fatalf("error type = %T (%v), want *ErrForbiddenField", err, err)
	}
	// The reported key may be the parent table ("accounts.alice") or
	// the leaf ("accounts.alice.opt_in") depending on how the decoder
	// walks the tree; either is correct as long as it begins with
	// "accounts." so the operator sees which surface they touched.
	if !strings.HasPrefix(ffe.Key, "accounts") {
		t.Errorf("ErrForbiddenField.Key = %q, want a key beginning with \"accounts\"", ffe.Key)
	}
	if ffe.Hint == "" {
		t.Error("ErrForbiddenField.Hint is empty")
	}
	if !strings.Contains(strings.ToLower(ffe.Hint), "fauna") {
		t.Errorf("hint should mention the Fauna app UI; got %q", ffe.Hint)
	}
}

// TestRejectsBridgeRole asserts that `role = "mta"` at the
// operator-hatch returns ErrForbiddenField. The bridge's role is
// resolved by nest from the enrolled service-user keypair (see
// fauna.bridges.whoami); the retired --mode flag is not coming back via
// a config file either.
func TestRejectsBridgeRole(t *testing.T) {
	assertForbiddenField(t,
		`role = "mta"`,
		"role",
	)
}

// TestErrForbiddenFieldMessageShape pins the human-readable error
// shape: the message must name the offending key, point at the Fauna
// admin UI, and not leak any TOML-parser internals.
func TestErrForbiddenFieldMessageShape(t *testing.T) {
	e := &ErrForbiddenField{
		Key:  "dkim_selector",
		Hint: "DKIM keys are provisioned via the admin pane → Mail → DKIM in your Fauna app.",
	}
	msg := e.Error()
	if !strings.Contains(msg, "dkim_selector") {
		t.Errorf("error message = %q; should mention the key", msg)
	}
	if !strings.Contains(strings.ToLower(msg), "fauna admin ui") {
		t.Errorf("error message = %q; should mention the Fauna admin UI", msg)
	}
	if !strings.Contains(msg, "forbidden") {
		t.Errorf("error message = %q; should label the field forbidden", msg)
	}
}

// TestLoadFromReaderNil pins the defensive nil-reader rejection; the
// caller should never pass a nil reader, but the parse-boundary error
// surface is small enough to enforce it explicitly.
func TestLoadFromReaderNil(t *testing.T) {
	hatch, err := LoadFromReader(nil)
	if err == nil {
		t.Fatalf("LoadFromReader(nil): expected error, got nil hatch=%+v", hatch)
	}
	if hatch != nil {
		t.Errorf("LoadFromReader(nil): expected nil hatch, got %+v", hatch)
	}
}
