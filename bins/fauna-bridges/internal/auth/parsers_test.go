package auth

import (
	"errors"
	"strings"
	"testing"
)

func TestBuildPlainPayloadRoundTrip(t *testing.T) {
	cases := []struct {
		name                       string
		authzID, authcID, password string
		wantUser, wantPass         string
		wantErr                    error
	}{
		{
			name:     "empty authzid",
			authzID:  "",
			authcID:  "alice@example.com",
			password: "hunter2",
			wantUser: "alice@example.com",
			wantPass: "hunter2",
		},
		{
			name:     "authzid equals authcid",
			authzID:  "alice@example.com",
			authcID:  "alice@example.com",
			password: "hunter2",
			wantUser: "alice@example.com",
			wantPass: "hunter2",
		},
		{
			name:     "password contains NUL byte",
			authzID:  "",
			authcID:  "alice@example.com",
			password: "p\x00ass", // ParsePlainPayload's SplitN("\x00", 3) keeps the trailing piece intact
			wantUser: "alice@example.com",
			wantPass: "p\x00ass",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			payload := BuildPlainPayload(tc.authzID, tc.authcID, tc.password)
			user, pass, err := ParsePlainPayload(payload)
			if err != nil {
				t.Fatalf("ParsePlainPayload: unexpected err %v", err)
			}
			if user != tc.wantUser {
				t.Errorf("user = %q, want %q", user, tc.wantUser)
			}
			if pass != tc.wantPass {
				t.Errorf("pass = %q, want %q", pass, tc.wantPass)
			}
		})
	}
}

func TestParsePlainPayloadRejectsMalformed(t *testing.T) {
	cases := []struct {
		name    string
		payload string
		wantErr error
	}{
		{"empty payload", "", ErrPlainMalformed},
		{"single NUL", "\x00", ErrPlainMalformed},
		{"two pieces only", "user\x00pass", ErrPlainMalformed},
		{"authzid mismatch", BuildPlainPayload("eve@evil", "alice@example", "pw"), ErrAuthzIDMismatch},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			_, _, err := ParsePlainPayload(tc.payload)
			if !errors.Is(err, tc.wantErr) {
				t.Fatalf("err = %v, want %v", err, tc.wantErr)
			}
		})
	}
}

func TestBuildOAuthBearerPayloadRoundTrip(t *testing.T) {
	payload := BuildOAuthBearerPayload("alice@example.com", "deterministic-token")
	user, token, ok := ParseOAuthBearerPayload(payload)
	if !ok {
		t.Fatal("round-trip must parse")
	}
	if user != "alice@example.com" {
		t.Errorf("user = %q", user)
	}
	if token != "deterministic-token" {
		t.Errorf("token = %q", token)
	}
}

func TestParseOAuthBearerPayloadRejectsMalformed(t *testing.T) {
	cases := []struct {
		name    string
		payload string
	}{
		{"empty payload", ""},
		{"missing GS2 header", "a=alice,\x01auth=Bearer tok\x01\x01"},
		{"missing comma after authzid", "n,a=alice\x01auth=Bearer tok\x01\x01"},
		{"missing auth kvp", "n,a=alice,\x01host=mail.example.com\x01\x01"},
		{"missing Bearer prefix", "n,a=alice,\x01auth=tok\x01\x01"},
		{"empty username", "n,a=,\x01auth=Bearer tok\x01\x01"},
		{"empty token", "n,a=alice,\x01auth=Bearer \x01\x01"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			_, _, ok := ParseOAuthBearerPayload(tc.payload)
			if ok {
				t.Fatal("malformed payload must not parse")
			}
		})
	}
}

func TestParseOAuthBearerPayloadIgnoresExtraKvps(t *testing.T) {
	// RFC 7628 lets clients add host=/port= advisory kvps; the bridge
	// trusts TLS for host identity and ignores them. The parser must
	// not break on their presence and must still extract auth=Bearer.
	payload := "n,a=alice@example.com,\x01host=mail.example.com\x01port=587\x01auth=Bearer abc123\x01\x01"
	user, token, ok := ParseOAuthBearerPayload(payload)
	if !ok {
		t.Fatal("extra kvps must not break parse")
	}
	if user != "alice@example.com" {
		t.Errorf("user = %q", user)
	}
	if token != "abc123" {
		t.Errorf("token = %q", token)
	}
}

func TestParseOAuthBearerPayloadCaseInsensitiveBearer(t *testing.T) {
	// RFC 6750 says the bearer scheme is case-insensitive ("Bearer" /
	// "bearer" / "BEARER"). Real-world MUAs vary; the parser must accept
	// all variants.
	for _, prefix := range []string{"Bearer ", "bearer ", "BEARER "} {
		payload := "n,a=alice,\x01auth=" + prefix + "tok\x01\x01"
		_, token, ok := ParseOAuthBearerPayload(payload)
		if !ok || token != "tok" {
			t.Errorf("prefix %q: got ok=%v token=%q", prefix, ok, token)
		}
	}
}

func TestSplitEmail(t *testing.T) {
	cases := []struct {
		name                  string
		addr                  string
		wantLocal, wantDomain string
		wantOK                bool
	}{
		{"simple", "alice@example.com", "alice", "example.com", true},
		{"subdomain", "bob@mail.example.com", "bob", "mail.example.com", true},
		{"local part with dot", "first.last@example.com", "first.last", "example.com", true},
		{"local part with at via quoting", "weird@thing@example.com", "weird@thing", "example.com", true},
		{"missing at", "alice", "", "", false},
		{"empty local part", "@example.com", "", "", false},
		{"empty domain", "alice@", "", "", false},
		{"empty", "", "", "", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			local, domain, ok := SplitEmail(tc.addr)
			if ok != tc.wantOK {
				t.Fatalf("ok = %v, want %v", ok, tc.wantOK)
			}
			if !ok {
				return
			}
			if local != tc.wantLocal {
				t.Errorf("local = %q, want %q", local, tc.wantLocal)
			}
			if domain != tc.wantDomain {
				t.Errorf("domain = %q, want %q", domain, tc.wantDomain)
			}
		})
	}
}

func TestSplitEmailDefault(t *testing.T) {
	cases := []struct {
		name                  string
		addr, defaultDomain   string
		wantLocal, wantDomain string
		wantOK                bool
	}{
		// A full address ignores the default entirely (same as SplitEmail).
		// The default domain here is deliberately DISTINCT from the address's
		// own domain (example.net vs. example.com) — using the codebase's own
		// scrub-target domain would collapse the contrast under the publish
		// scrub, silently losing this case's discriminating power in the
		// shipped tree.
		{"full address ignores default", "alice@example.com", "example.net", "alice", "example.com", true},
		// An arbitrary / locator domain part passes straight through — on a
		// bare-IP nest the user logs in as `test@192.168.1.57`; nest resolves
		// it by handle because the IP isn't a registered mail-domain (Change A).
		{"locator/IP domain passes through", "test@192.168.1.57", "", "test", "192.168.1.57", true},
		// A bare username (no '@') — what macOS Calendar.app sends after
		// stripping the domain from a configured email — resolves under the
		// box's primary domain. The domain here is an arbitrary stand-in
		// (deliberately not the codebase's own scrub-target domain — see the
		// first case's comment above; the real production domain here would
		// scrub to the address domain used above, colliding function-wide).
		{"bare username defaults the domain", "alice", "example.net", "alice", "example.net", true},
		{"bare username with +suffix keeps the local part", "alice+phone", "example.net", "alice+phone", "example.net", true},
		// No primary domain claimed yet (the domainless / bare-IP nest) → a bare
		// username now flows through with an EMPTY domain so it still reaches the
		// validate_recipient RPC, where nest resolves it against the unique
		// handle→actor store (Change A — the locator-honoring relaxation; the old
		// "empty default ⇒ malformed" strict behavior is retired).
		{"bare username with empty default flows through domainless", "test", "", "test", "", true},
		{"bare username with +suffix and empty default flows through", "test+phone", "", "test+phone", "", true},
		// Genuinely malformed shapes are NOT rescued by the default — only a
		// no-'@' bare username is.
		{"empty local part not rescued", "@example.com", "example.net", "", "", false},
		{"empty domain not rescued", "alice@", "example.net", "", "", false},
		{"empty string not rescued", "", "example.net", "", "", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			local, domain, ok := SplitEmailDefault(tc.addr, tc.defaultDomain)
			if ok != tc.wantOK {
				t.Fatalf("ok = %v, want %v", ok, tc.wantOK)
			}
			if !ok {
				return
			}
			if local != tc.wantLocal {
				t.Errorf("local = %q, want %q", local, tc.wantLocal)
			}
			if domain != tc.wantDomain {
				t.Errorf("domain = %q, want %q", domain, tc.wantDomain)
			}
		})
	}
}

func TestCredentialFromLocalPart(t *testing.T) {
	cases := []struct {
		name              string
		local             string
		wantBase, wantCID string
	}{
		{"bare → default", "alice", "alice", "default"},
		{"sub-address → credential", "alice+phone", "alice", "phone"},
		{"kebab credential id", "alice+work-laptop", "alice", "work-laptop"},
		{"first plus wins", "alice+a+b", "alice", "a+b"},
		{"empty suffix falls back", "alice+", "alice", "default"},
		{"dotted base", "first.last+phone", "first.last", "phone"},
		{"explicit default suffix", "alice+default", "alice", "default"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			base, cid := CredentialFromLocalPart(tc.local)
			if base != tc.wantBase {
				t.Errorf("base = %q, want %q", base, tc.wantBase)
			}
			if cid != tc.wantCID {
				t.Errorf("credentialID = %q, want %q", cid, tc.wantCID)
			}
		})
	}
}

// principalAndCredential mirrors how every bridge AUTH surface derives the
// lockout key from a raw presented username: SplitEmailDefault → first '+'
// credential split → PrincipalKey. The lockout's fine/mid buckets key on the
// returned (principal, credentialID) pair.
func principalAndCredential(t *testing.T, presented, primaryDomain string) (principal, credentialID string) {
	t.Helper()
	local, domain, ok := SplitEmailDefault(presented, primaryDomain)
	if !ok {
		t.Fatalf("SplitEmailDefault(%q, %q) not ok", presented, primaryDomain)
	}
	base, cid := CredentialFromLocalPart(local)
	return PrincipalKey(base, domain), cid
}

func TestPrincipalKeyCollapsesEquivalentPresentedForms(t *testing.T) {
	const pd = "example.com"

	// All of these resolve to the SAME credential — base "alice", credential
	// "default" — so they must key ONE lockout bucket. Before the § B7 fix the
	// raw presented username was the key, so each form opened a fresh bucket and
	// multiplied the per-credential AUTH-failure allowance.
	sameCredential := []string{"alice", "alice+", "alice+default", "alice@example.com", "alice@Example.COM"}
	wantP, wantC := principalAndCredential(t, sameCredential[0], pd)
	for _, f := range sameCredential[1:] {
		p, c := principalAndCredential(t, f, pd)
		if p != wantP || c != wantC {
			t.Errorf("form %q keyed (%q,%q); want same bucket as %q (%q,%q)",
				f, p, c, sameCredential[0], wantP, wantC)
		}
	}

	// A genuinely DIFFERENT credential of the same account must NOT collapse —
	// the per-credential allowance is by design.
	pPhone, cPhone := principalAndCredential(t, "alice+phone", pd)
	if pPhone == wantP && cPhone == wantC {
		t.Errorf("alice+phone collapsed into the (alice,default) bucket — distinct credentials must keep distinct buckets")
	}

	// A different base mailbox is a different account → different bucket.
	pBob, _ := principalAndCredential(t, "bob", pd)
	if pBob == wantP {
		t.Errorf("bob and alice keyed the same principal %q", pBob)
	}
}

func TestPrincipalKeyDomainlessAndCaseFolding(t *testing.T) {
	// The bare-IP nest (empty primary domain): SplitEmailDefault passes a bare
	// username through domainless, so the principal is just the base.
	if got := PrincipalKey("test", ""); got != "test" {
		t.Errorf("PrincipalKey(test, \"\") = %q, want %q", got, "test")
	}
	// DNS is case-insensitive: the domain folds, the local-part does not.
	if PrincipalKey("alice", "Example.COM") != PrincipalKey("alice", "example.com") {
		t.Errorf("domain case must fold")
	}
	if PrincipalKey("Alice", "example.com") == PrincipalKey("alice", "example.com") {
		t.Errorf("local-part case must NOT fold (nest is the authority on local-part equivalence)")
	}
}

func TestRedactedFailReason(t *testing.T) {
	if got := RedactedFailReason(nil); got != "" {
		t.Errorf("nil error: got %q", got)
	}
	if got := RedactedFailReason(errors.New("AEAD verify failed")); got != "AEAD verify failed" {
		t.Errorf("short: got %q", got)
	}
	long := strings.Repeat("x", 500)
	got := RedactedFailReason(errors.New(long))
	if len(got) > 250 {
		t.Errorf("long reason not capped, len=%d", len(got))
	}
	if !strings.HasSuffix(got, "…") {
		t.Errorf("long reason missing ellipsis: %q", got[len(got)-5:])
	}
}
