package mda

import (
	"context"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/dav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// tagHandler is a comparable stub http.Handler carrying an identity tag, so a
// davMounts test can assert which handler landed at which mux pattern.
type tagHandler struct{ tag string }

func (tagHandler) ServeHTTP(http.ResponseWriter, *http.Request) {}

// TestRunReturnsWhenContextCancelled — Run idles until ctx is
// cancelled and returns nil cleanly. With all three DAV/mail protocols
// disabled (MailEnabled + CalDAVEnabled + CardDAVEnabled false), the "nothing
// enabled, idle gracefully" gate fires. A regression here (e.g. blocking on a
// select that never closes) would hang the SIGTERM path in main.go.
func TestRunReturnsWhenContextCancelled(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		done <- Run(ctx, Deps{
			Snapshot: wsrpc.ConfigSnapshot{
				MailEnabled:    false, // ← all three disabled → idle gate fires
				CalDAVEnabled:  false,
				CardDAVEnabled: false,
				LocalDomains:   []string{"test.example.com"},
				PrimaryDomain:  "test.example.com",
			},
			BridgeID: "test-mda-1",
		})
	}()

	time.Sleep(50 * time.Millisecond)
	cancel()

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Run: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Run did not return within 1s of ctx cancel")
	}
}

// TestRunDoesNotIdleWhenLocalDomainsEmpty — the any-locator design
// (tracked internally): a
// domainless / bare-IP / localhost nest has no mail_domains rows, but CalDAV +
// local IMAP STILL serve — the MDA terminates its own TLS with nest's
// self-signed FLOOR cert (main.go builds the TLS provider even with an empty
// PrimaryDomain) and local login resolves the bare handle via the handle→actor
// store (Change A). So Run must NOT idle when LocalDomains is empty; it falls
// through to bind the enabled protocols. We prove it reached the post-idle path
// by leaving TLSProvider nil with mail enabled: instead of idling (the
// pre-2026-06-18 behavior, which returned nil), Run now hits the TLSProvider
// wiring check and returns a non-nil error. (This test asserted the OPPOSITE
// before the any-locator serving fix — the behavior that fix overturns. The MTA
// keeps its own no-domain idle gate: external email genuinely needs a domain.)
func TestRunDoesNotIdleWhenLocalDomainsEmpty(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	err := Run(ctx, Deps{
		Snapshot: wsrpc.ConfigSnapshot{
			MailEnabled:   true,
			LocalDomains:  []string{}, // ← no longer an idle gate (any-locator)
			PrimaryDomain: "",
		},
		BridgeID:              "test-mda-1",
		CalDAVListenHTTPS:     ":0",
		IMAPListenImplicitTLS: ":0",
		IMAPListenStartTLS:    ":0",
		TLSProvider:           nil, // ← reached only if Run did NOT idle on empty LocalDomains
	})
	if err == nil {
		t.Fatal("Run with empty LocalDomains must NOT idle (any-locator): it should fall through to the bind path and hit the TLSProvider wiring check, got nil")
	}
	if !strings.Contains(err.Error(), "TLSProvider") {
		t.Fatalf("expected the post-idle TLSProvider wiring check (proving Run no longer idles on empty LocalDomains), got: %v", err)
	}
}

// TestRunErrorsOnNilTLSProvider — when mail is enabled AND a domain
// is ratified AND every listen address is wired, the listeners require
// terminated TLS (CalDAV 443 is HTTPS; IMAP 993 is implicit-TLS via
// tls.NewListener; IMAP 143 hands tls.Config to the library for
// STARTTLS upgrade). A nil TLSProvider at that point is a wiring bug,
// not a transient condition, so Run returns an error immediately
// rather than idling. Mirrors mta.Run's TLSProvider validation.
func TestRunErrorsOnNilTLSProvider(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	err := Run(ctx, Deps{
		Snapshot: wsrpc.ConfigSnapshot{
			MailEnabled:   true,
			LocalDomains:  []string{"test.example.com"},
			PrimaryDomain: "test.example.com",
		},
		BridgeID:              "test-mda-1",
		CalDAVListenHTTPS:     ":0",
		IMAPListenImplicitTLS: ":0",
		IMAPListenStartTLS:    ":0",
		TLSProvider:           nil, // ← gate condition
	})
	if err == nil {
		t.Fatal("Run with nil TLSProvider should return an error, got nil")
	}
	if !strings.Contains(err.Error(), "TLSProvider") {
		t.Fatalf("error should mention TLSProvider, got: %v", err)
	}
}

// TestRunErrorsOnEmptyCalDAVListenHTTPS — when mail is enabled AND a
// domain is ratified, an empty CalDAVListenHTTPS is a wiring bug at
// main.go. Run returns an error rather than silently skipping the
// listener (which would surface as "CalDAV traffic to :443 hits
// nothing" — a harder bug to triage than a startup error).
func TestRunErrorsOnEmptyCalDAVListenHTTPS(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	err := Run(ctx, Deps{
		Snapshot: wsrpc.ConfigSnapshot{
			MailEnabled:   true,
			CalDAVEnabled: true, // ← CalDAV enabled, so its addr gate is live
			LocalDomains:  []string{"test.example.com"},
			PrimaryDomain: "test.example.com",
		},
		BridgeID:              "test-mda-1",
		CalDAVListenHTTPS:     "", // ← gate condition
		IMAPListenImplicitTLS: ":0",
		IMAPListenStartTLS:    ":0",
	})
	if err == nil {
		t.Fatal("Run with empty CalDAVListenHTTPS should return an error, got nil")
	}
	if !strings.Contains(err.Error(), "CalDAVListenHTTPS") {
		t.Fatalf("error should mention CalDAVListenHTTPS, got: %v", err)
	}
}

// TestRunErrorsOnEmptyIMAPListenImplicitTLS — symmetric to
// TestRunErrorsOnEmptyCalDAVListenHTTPS, for the IMAPS 993 listener.
// An empty IMAPListenImplicitTLS is a wiring bug at main.go (the
// default should already have been applied); Run returns an error
// rather than silently binding only CalDAV + the STARTTLS IMAP socket.
func TestRunErrorsOnEmptyIMAPListenImplicitTLS(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	err := Run(ctx, Deps{
		Snapshot: wsrpc.ConfigSnapshot{
			MailEnabled:   true,
			LocalDomains:  []string{"test.example.com"},
			PrimaryDomain: "test.example.com",
		},
		BridgeID:              "test-mda-1",
		CalDAVListenHTTPS:     ":0",
		IMAPListenImplicitTLS: "", // ← gate condition
		IMAPListenStartTLS:    ":0",
	})
	if err == nil {
		t.Fatal("Run with empty IMAPListenImplicitTLS should return an error, got nil")
	}
	if !strings.Contains(err.Error(), "IMAPListenImplicitTLS") {
		t.Fatalf("error should mention IMAPListenImplicitTLS, got: %v", err)
	}
}

// TestRunErrorsOnEmptyIMAPListenStartTLS — symmetric to the IMAPS
// gate above, for the IMAP 143 STARTTLS listener. Same rationale: an
// empty addr is a wiring bug, not a "skip this listener" signal.
func TestRunErrorsOnEmptyIMAPListenStartTLS(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	err := Run(ctx, Deps{
		Snapshot: wsrpc.ConfigSnapshot{
			MailEnabled:   true,
			LocalDomains:  []string{"test.example.com"},
			PrimaryDomain: "test.example.com",
		},
		BridgeID:              "test-mda-1",
		CalDAVListenHTTPS:     ":0",
		IMAPListenImplicitTLS: ":0",
		IMAPListenStartTLS:    "", // ← gate condition
	})
	if err == nil {
		t.Fatal("Run with empty IMAPListenStartTLS should return an error, got nil")
	}
	if !strings.Contains(err.Error(), "IMAPListenStartTLS") {
		t.Fatalf("error should mention IMAPListenStartTLS, got: %v", err)
	}
}

// TestRunIgnoresEmptyIMAPAddrsWhenMailDisabled — CalDAV-only deployment
// (CalDAVEnabled true, MailEnabled false): the IMAP listen addrs are empty but
// must NOT trip the wiring-bug gate, because the IMAP listeners aren't bound at
// all. Proven by passing a nil TLSProvider with empty IMAP addrs and a valid
// CalDAV addr: Run falls through the (skipped) IMAP addr checks to the
// TLSProvider gate, so the error mentions TLSProvider — not IMAPListen*. This
// is the per-protocol gating from caldav-server.md § Independent enablement.
func TestRunIgnoresEmptyIMAPAddrsWhenMailDisabled(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	err := Run(ctx, Deps{
		Snapshot: wsrpc.ConfigSnapshot{
			MailEnabled:   false, // ← IMAP listeners not bound
			CalDAVEnabled: true,
			LocalDomains:  []string{"test.example.com"},
			PrimaryDomain: "test.example.com",
		},
		BridgeID:              "test-mda-1",
		CalDAVListenHTTPS:     ":0",
		IMAPListenImplicitTLS: "", // empty, but ignored (mail off)
		IMAPListenStartTLS:    "", // empty, but ignored (mail off)
		TLSProvider:           nil,
	})
	if err == nil {
		t.Fatal("Run with CalDAV enabled + nil TLSProvider should error, got nil")
	}
	if strings.Contains(err.Error(), "IMAPListen") {
		t.Fatalf("empty IMAP addrs must be ignored when mail is disabled, got: %v", err)
	}
	if !strings.Contains(err.Error(), "TLSProvider") {
		t.Fatalf("error should fall through to TLSProvider, got: %v", err)
	}
}

// TestRunIgnoresEmptyCalDAVAddrWhenCalDAVDisabled — the symmetric email-only
// case (MailEnabled true, CalDAVEnabled false): an empty CalDAVListenHTTPS must
// be ignored because the CalDAV listener isn't bound. Same probe: nil
// TLSProvider with an empty CalDAV addr + valid IMAP addrs → the error mentions
// TLSProvider, not CalDAVListenHTTPS.
func TestRunIgnoresEmptyCalDAVAddrWhenCalDAVDisabled(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	err := Run(ctx, Deps{
		Snapshot: wsrpc.ConfigSnapshot{
			MailEnabled:   true,
			CalDAVEnabled: false, // ← CalDAV listener not bound
			LocalDomains:  []string{"test.example.com"},
			PrimaryDomain: "test.example.com",
		},
		BridgeID:              "test-mda-1",
		CalDAVListenHTTPS:     "", // empty, but ignored (caldav off)
		IMAPListenImplicitTLS: ":0",
		IMAPListenStartTLS:    ":0",
		TLSProvider:           nil,
	})
	if err == nil {
		t.Fatal("Run with mail enabled + nil TLSProvider should error, got nil")
	}
	if strings.Contains(err.Error(), "CalDAVListenHTTPS") {
		t.Fatalf("empty CalDAV addr must be ignored when CalDAV is disabled, got: %v", err)
	}
	if !strings.Contains(err.Error(), "TLSProvider") {
		t.Fatalf("error should fall through to TLSProvider, got: %v", err)
	}
}

// TestCalDAVPortRebindNeeded locks the admin-settable-port rebind decision
// (caldav-server.md § Network exposure): an admin-port-driven listener rebinds
// when the effective port changes; a hatch-pinned box (admin port doesn't drive
// the bind) never rebinds on a port change; and a 0 (the defensive port
// clamp) normalizes to the default so it reads as no-change
// against a defaulted startup port — never a spurious restart.
func TestCalDAVPortRebindNeeded(t *testing.T) {
	t.Parallel()
	const def = wsrpc.DefaultCalDAVPort // 8443
	cases := []struct {
		name        string
		adminDriven bool
		startPort   uint16 // already-effective startup port
		newPort     uint16 // raw snapshot port (normalized inside)
		want        bool
	}{
		{"admin-driven, port changed → rebind", true, def, 9443, true},
		{"admin-driven, port unchanged → no rebind", true, def, def, false},
		{"admin-driven, new 0 (no field) vs default start → no rebind", true, def, 0, false},
		{"admin-driven, distinct change → rebind", true, 9443, 8443, true},
		{"hatch-pinned, port changed → NO rebind", false, def, 9443, false},
		{"hatch-pinned, port unchanged → no rebind", false, def, def, false},
	}
	for _, tc := range cases {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			if got := caldavPortRebindNeeded(tc.adminDriven, tc.startPort, tc.newPort); got != tc.want {
				t.Errorf("caldavPortRebindNeeded(%v, %d, %d) = %v, want %v",
					tc.adminDriven, tc.startPort, tc.newPort, got, tc.want)
			}
		})
	}
}

// TestDavMounts locks the shared-443 mount-set decision (davMounts): CalDAV
// (when on) owns the `/` catch-all so its root principal is reachable; CardDAV
// (when on) owns `/carddav/`; and a CONTACTS-ONLY box (CardDAV on, CalDAV off)
// additionally mounts CardDAV at `/` so its own root principal is reachable with
// no CalDAV catch-all present. Asserted as pattern→handler-identity pairs so a
// future edit that drops a mount (breaking discovery) or crosses the wires
// (routing CalDAV traffic to the CardDAV chain) fails here.
func TestDavMounts(t *testing.T) {
	t.Parallel()
	cal := tagHandler{tag: "caldav"}
	card := tagHandler{tag: "carddav"}
	web := tagHandler{tag: "webdav"}

	tag := func(h http.Handler) string {
		if h == nil {
			return "<nil>"
		}
		return h.(tagHandler).tag
	}
	assertMounts := func(t *testing.T, got []dav.Mount, want map[string]string) {
		t.Helper()
		if len(got) != len(want) {
			t.Fatalf("mount count = %d, want %d (%+v)", len(got), len(want), got)
		}
		seen := map[string]string{}
		for _, m := range got {
			if _, dup := seen[m.Pattern]; dup {
				t.Fatalf("pattern %q mounted twice", m.Pattern)
			}
			seen[m.Pattern] = tag(m.Handler)
		}
		for pat, wantTag := range want {
			if seen[pat] != wantTag {
				t.Errorf("pattern %q → %q, want %q", pat, seen[pat], wantTag)
			}
		}
	}

	t.Run("caldav only → CalDAV owns /", func(t *testing.T) {
		assertMounts(t, davMounts(true, false, false, cal, nil, nil),
			map[string]string{"/": "caldav"})
	})
	t.Run("carddav only → CardDAV owns /carddav/ and / (contacts-only root)", func(t *testing.T) {
		assertMounts(t, davMounts(false, true, false, nil, card, nil),
			map[string]string{"/carddav/": "carddav", "/": "carddav"})
	})
	// With CalDAV owning the root, CardDAV's own service discovery must still
	// reach the CardDAV chain: the apex sends a contacts app to the mail
	// host's /.well-known/carddav, and the CalDAV chain would answer it with an
	// empty multistatus — a dead end for automatic setup.
	t.Run("both → CalDAV owns /, CardDAV owns /carddav/ and its well-known", func(t *testing.T) {
		assertMounts(t, davMounts(true, true, false, cal, card, nil),
			map[string]string{"/": "caldav", "/carddav/": "carddav", "/.well-known/carddav": "carddav"})
	})
	t.Run("webdav only → WebDAV owns /webdav/ (no root; no discovery)", func(t *testing.T) {
		assertMounts(t, davMounts(false, false, true, nil, nil, web),
			map[string]string{"/webdav/": "webdav"})
	})
	t.Run("all three → CalDAV /, CardDAV /carddav/ + its well-known, WebDAV /webdav/", func(t *testing.T) {
		assertMounts(t, davMounts(true, true, true, cal, card, web),
			map[string]string{"/": "caldav", "/carddav/": "carddav", "/.well-known/carddav": "carddav", "/webdav/": "webdav"})
	})
	t.Run("carddav+webdav (no caldav) → CardDAV owns / and /carddav/, WebDAV /webdav/", func(t *testing.T) {
		assertMounts(t, davMounts(false, true, true, nil, card, web),
			map[string]string{"/": "carddav", "/carddav/": "carddav", "/webdav/": "webdav"})
	})
	t.Run("neither → empty (caller guards with if caldavOn||carddavOn||webdavOn)", func(t *testing.T) {
		if got := davMounts(false, false, false, nil, nil, nil); len(got) != 0 {
			t.Errorf("davMounts(false,false,false) = %+v, want empty", got)
		}
	})
}

// TestRunErrorsOnEmptyCalDAVListenHTTPSWhenOnlyCardDAV — the shared DAV 443 addr
// is required whenever CalDAV *or* CardDAV is on (both ride it), so a
// contacts-only deployment (CardDAV enabled, CalDAV disabled) with an empty
// CalDAVListenHTTPS is the same wiring bug as the CalDAV case: Run must fail
// loudly rather than silently skip the listener. Sibling to
// TestRunErrorsOnEmptyCalDAVListenHTTPS, on the CardDAV gate.
func TestRunErrorsOnEmptyCalDAVListenHTTPSWhenOnlyCardDAV(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	err := Run(ctx, Deps{
		Snapshot: wsrpc.ConfigSnapshot{
			MailEnabled:    false,
			CalDAVEnabled:  false,
			CardDAVEnabled: true, // ← contacts-only: CardDAV rides the shared 443 addr
			LocalDomains:   []string{"test.example.com"},
			PrimaryDomain:  "test.example.com",
		},
		BridgeID:              "test-mda-1",
		CalDAVListenHTTPS:     "", // ← gate condition (the shared DAV addr)
		IMAPListenImplicitTLS: ":0",
		IMAPListenStartTLS:    ":0",
	})
	if err == nil {
		t.Fatal("Run with CardDAV enabled + empty CalDAVListenHTTPS should return an error, got nil")
	}
	if !strings.Contains(err.Error(), "CalDAVListenHTTPS") {
		t.Fatalf("error should mention CalDAVListenHTTPS (the shared DAV addr), got: %v", err)
	}
}

// TestRunBindsSharedDAVListenerForCardDAVOnly — a contacts-only deployment
// (MailEnabled false, CalDAVEnabled false, CardDAVEnabled true) must NOT hit the
// all-disabled idle gate; it falls through to the bind path. Proven the same way
// as the mail/caldav gate tests: a nil TLSProvider with a valid shared 443 addr
// makes Run reach the TLSProvider wiring check (which only runs post-idle), so
// the error mentions TLSProvider — confirming CardDAV alone triggers the bind.
func TestRunBindsSharedDAVListenerForCardDAVOnly(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	err := Run(ctx, Deps{
		Snapshot: wsrpc.ConfigSnapshot{
			MailEnabled:    false,
			CalDAVEnabled:  false,
			CardDAVEnabled: true, // ← only CardDAV on
			LocalDomains:   []string{"test.example.com"},
			PrimaryDomain:  "test.example.com",
		},
		BridgeID:          "test-mda-1",
		CalDAVListenHTTPS: ":0", // shared DAV addr wired
		TLSProvider:       nil,  // ← reached only if Run did NOT idle on carddav-only
	})
	if err == nil {
		t.Fatal("Run with only CardDAV enabled must NOT idle: it should reach the bind path and hit the TLSProvider wiring check, got nil")
	}
	if !strings.Contains(err.Error(), "TLSProvider") {
		t.Fatalf("expected the post-idle TLSProvider check (proving CardDAV alone triggers the bind path), got: %v", err)
	}
}
