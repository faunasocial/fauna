package davauth

import (
	"log/slog"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/authlock"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// okProbe is a downstream handler that just 200s — used to detect when the
// auth middleware lets a request through (success path) vs short-circuits.
func okProbe() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
	})
}

// caldavAuthReq builds a Basic-Auth PROPFIND for the fixture actor with the
// given password + source IP and runs it through `mw`.
func caldavAuthReq(mw http.Handler, password, sourceIP string) *httptest.ResponseRecorder {
	req := httptest.NewRequest("PROPFIND",
		"/caldav/"+fixtureLocalPart+"@"+fixtureDomain+"/", nil)
	req.RemoteAddr = sourceIP + ":5555"
	req.SetBasicAuth(fixtureLocalPart+"@"+fixtureDomain, password)
	rec := httptest.NewRecorder()
	mw.ServeHTTP(rec, req)
	return rec
}

func resolvableCaller(t *testing.T) *mockCaller {
	t.Helper()
	return &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            mustReadFixture(t, "wrapped_msek.bin"),
		mlsPubkey:              fixtureMLSPubkey,
		indexKey:               fixtureIndexKey,
		mlsSnapshotBlob:        nil, // AUTH succeeds without a snapshot
	}
}

// TestCalDAVLockoutShortCircuitsBeforeKDF pins the headline D4/M1 fix for
// CalDAV Basic-Auth: once a (username, source-IP) has racked up `limit`
// failures, the next request is refused with 401 BEFORE validate_recipient
// and the Argon2id unwrap — so a brute-forcer can't keep paying the bridge's
// KDF cost. The lockout is shared across requests via the atomic pointer the
// Server hands the middleware.
func TestCalDAVLockoutShortCircuitsBeforeKDF(t *testing.T) {
	lockout := &atomic.Pointer[authlock.Lockout]{}
	lockout.Store(authlock.New(3, time.Minute, nil))

	newMW := func() (*mockCaller, http.Handler) {
		caller := resolvableCaller(t)
		return caller, NewMiddleware("fauna-caldav", okProbe(), caller, slog.Default(), lockout, nil)
	}

	// 3 failed requests from the same (user, IP) trip the lockout.
	for i := 0; i < 3; i++ {
		caller, mw := newMW()
		rec := caldavAuthReq(mw, "WRONG-PASSWORD", "203.0.113.7")
		if rec.Code != http.StatusUnauthorized {
			t.Fatalf("attempt %d: wrong password must 401, got %d", i+1, rec.Code)
		}
		if got := len(caller.callsOf(wsrpc.MethodValidateRecipient)); got != 1 {
			t.Fatalf("attempt %d: expected 1 validate_recipient, got %d", i+1, got)
		}
	}

	// 4th request is locked out: refused before any nest call / KDF.
	caller, mw := newMW()
	rec := caldavAuthReq(mw, "WRONG-PASSWORD", "203.0.113.7")
	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("locked attempt must 401, got %d", rec.Code)
	}
	if got := len(caller.callsOf(wsrpc.MethodValidateRecipient)); got != 0 {
		t.Fatalf("locked attempt called validate_recipient %d times; must short-circuit before the KDF", got)
	}
	if got := len(caller.callsOf(wsrpc.MethodReportAuthEvent)); got != 0 {
		t.Fatalf("locked attempt fired report_auth_event %d times; lockout branch must skip the audit", got)
	}
}

// TestCalDAVLockoutResetsOnSuccess pins that a correct password clears the
// counter, so earlier typos never lock out a legitimate user.
func TestCalDAVLockoutResetsOnSuccess(t *testing.T) {
	lockout := &atomic.Pointer[authlock.Lockout]{}
	lockout.Store(authlock.New(3, time.Minute, nil))

	newMW := func() (*mockCaller, http.Handler) {
		caller := resolvableCaller(t)
		return caller, NewMiddleware("fauna-caldav", okProbe(), caller, slog.Default(), lockout, nil)
	}

	for i := 0; i < 2; i++ {
		_, mw := newMW()
		caldavAuthReq(mw, "WRONG-PASSWORD", "203.0.113.7")
	}
	// One success resets the (user, IP) counter.
	_, mwOK := newMW()
	if rec := caldavAuthReq(mwOK, string(fixturePlainPassword), "203.0.113.7"); rec.Code != http.StatusOK {
		t.Fatalf("good password after 2 typos must 200, got %d", rec.Code)
	}
	// Two more fails must NOT lock (counter reset; only 2 accrued).
	for i := 0; i < 2; i++ {
		caller, mw := newMW()
		caldavAuthReq(mw, "WRONG-PASSWORD", "203.0.113.7")
		if got := len(caller.callsOf(wsrpc.MethodValidateRecipient)); got != 1 {
			t.Fatalf("post-reset fail %d: lockout fired early (validate_recipient called %d times)", i+1, got)
		}
	}
}

// TestCalDAVLockoutKeysPerUsername pins that the lockout doesn't collapse all
// CalDAV traffic into one bucket: a tripped lockout for alice must not lock
// out bob from the same source IP (the key carries the username, so distinct
// accounts stay independent even when the IP dimension is constant behind the
// SNI router).
func TestCalDAVLockoutKeysPerUsername(t *testing.T) {
	lockout := &atomic.Pointer[authlock.Lockout]{}
	lockout.Store(authlock.New(2, time.Minute, nil))

	// Trip alice (2 fails).
	for i := 0; i < 2; i++ {
		caller := resolvableCaller(t)
		mw := NewMiddleware("fauna-caldav", okProbe(), caller, slog.Default(), lockout, nil)
		caldavAuthReq(mw, "WRONG-PASSWORD", "203.0.113.7")
	}

	// bob from the same IP must still reach the KDF (validate_recipient runs).
	bobCaller := &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            mustReadFixture(t, "wrapped_msek.bin"),
		mlsPubkey:              fixtureMLSPubkey,
		indexKey:               fixtureIndexKey,
	}
	mw := NewMiddleware("fauna-caldav", okProbe(), bobCaller, slog.Default(), lockout, nil)
	req := httptest.NewRequest("PROPFIND", "/caldav/bob@"+fixtureDomain+"/", nil)
	req.RemoteAddr = "203.0.113.7:5555"
	req.SetBasicAuth("bob@"+fixtureDomain, "WRONG-PASSWORD")
	rec := httptest.NewRecorder()
	mw.ServeHTTP(rec, req)
	if got := len(bobCaller.callsOf(wsrpc.MethodValidateRecipient)); got != 1 {
		t.Fatalf("bob must not be caught by alice's lockout; validate_recipient called %d times", got)
	}
}

// TestCalDAVReportAuthEventCarriesSourceIP pins the M1 audit-plumbing fix:
// CalDAV now stamps the request's source IP on report_auth_event. It used to
// send an empty source_ip, which nest rejects as malformed. Covers fail+ok.
func TestCalDAVReportAuthEventCarriesSourceIP(t *testing.T) {
	const wantIP = "203.0.113.9"

	sourceIPOf := func(rec recordedCall) string {
		var m map[string]any
		if err := cbor.Unmarshal(rec.body, &m); err != nil {
			t.Fatalf("decode report_auth_event body: %v", err)
		}
		ip, _ := m["source_ip"].(string)
		return ip
	}

	t.Run("fail", func(t *testing.T) {
		caller := resolvableCaller(t)
		mw := NewMiddleware("fauna-caldav", okProbe(), caller, slog.Default(), nil, nil)
		caldavAuthReq(mw, "WRONG-PASSWORD", wantIP)
		reports := caller.callsOf(wsrpc.MethodReportAuthEvent)
		if len(reports) != 1 {
			t.Fatalf("report_auth_event fired %d times on fail, want 1", len(reports))
		}
		if got := sourceIPOf(reports[0]); got != wantIP {
			t.Fatalf("fail report source_ip = %q, want %q", got, wantIP)
		}
	})

	t.Run("ok", func(t *testing.T) {
		caller := resolvableCaller(t)
		mw := NewMiddleware("fauna-caldav", okProbe(), caller, slog.Default(), nil, nil)
		if rec := caldavAuthReq(mw, string(fixturePlainPassword), wantIP); rec.Code != http.StatusOK {
			t.Fatalf("good password must 200, got %d", rec.Code)
		}
		reports := caller.callsOf(wsrpc.MethodReportAuthEvent)
		if len(reports) != 1 {
			t.Fatalf("report_auth_event fired %d times on ok, want 1", len(reports))
		}
		if got := sourceIPOf(reports[0]); got != wantIP {
			t.Fatalf("ok report source_ip = %q, want %q", got, wantIP)
		}
	})
}

// TestCalDAVBareUsernameResolvesUnderPrimaryDomain pins the macOS
// Calendar.app interop fix: CalendarAgent parses a configured `user@domain`,
// uses the domain for server discovery, and sends only the bare local part
// as the CalDAV Basic-auth username. The auth middleware must resolve that
// bare username under the box's PrimaryDomain instead of 401-ing it as
// "malformed username" — which 401-loops Apple Calendar into a "Connecting…"
// hang before discovery even starts (live evidence: example.com MDA logged
// `caldav: AUTH failed reason="malformed username"` against a correctly
// configured Calendar account). Mirrors how every mainstream CalDAV server
// (Nextcloud / SOGo / Radicale / Baïkal) accepts a bare username.
func TestCalDAVBareUsernameResolvesUnderPrimaryDomain(t *testing.T) {
	caller := resolvableCaller(t)
	pd := &atomic.Pointer[string]{}
	dom := fixtureDomain
	pd.Store(&dom)
	mw := NewMiddleware("fauna-caldav", okProbe(), caller, slog.Default(), nil, pd)

	// Bare username — no '@domain', exactly what macOS Calendar sends.
	req := httptest.NewRequest("PROPFIND", "/caldav/"+fixtureLocalPart+"/", nil)
	req.RemoteAddr = "203.0.113.7:5555"
	req.SetBasicAuth(fixtureLocalPart, string(fixturePlainPassword)) // "alice", not "alice@example.com"
	rec := httptest.NewRecorder()
	mw.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("bare username must authenticate under PrimaryDomain, got %d", rec.Code)
	}
	// validate_recipient must have been called with the defaulted domain.
	calls := caller.callsOf(wsrpc.MethodValidateRecipient)
	if len(calls) != 1 {
		t.Fatalf("validate_recipient fired %d times, want 1", len(calls))
	}
	var got map[string]any
	if err := cbor.Unmarshal(calls[0].body, &got); err != nil {
		t.Fatalf("decode validate_recipient body: %v", err)
	}
	if lp, _ := got["local_part"].(string); lp != fixtureLocalPart {
		t.Errorf("validate_recipient local_part = %q, want %q", lp, fixtureLocalPart)
	}
	if d, _ := got["domain"].(string); d != fixtureDomain {
		t.Errorf("validate_recipient domain = %q, want %q (PrimaryDomain default)", d, fixtureDomain)
	}
}

// TestCalDAVBareUsernameResolvesByHandleWithoutPrimaryDomain pins Change A (the
// any-locator design): with no PrimaryDomain configured (nil holder) — a
// domainless / bare-IP / localhost nest — a bare username is NOT rejected as
// "malformed" (the pre-Change-A strict `user@domain` rule this test asserted
// before). SplitEmailDefault passes it through with an EMPTY domain to
// validate_recipient, where nest resolves the local-part against the unique
// handle→actor store. So the request authenticates (200) and validate_recipient
// fires once with the bare local-part and an empty domain (the handle-fallback
// signal). See caldav-server.md § Any-locator local login (Change A) +
// auth.SplitEmailDefault (internal/auth/parsers.go).
func TestCalDAVBareUsernameResolvesByHandleWithoutPrimaryDomain(t *testing.T) {
	caller := resolvableCaller(t)
	mw := NewMiddleware("fauna-caldav", okProbe(), caller, slog.Default(), nil, nil) // no primary domain

	req := httptest.NewRequest("PROPFIND", "/caldav/"+fixtureLocalPart+"/", nil)
	req.RemoteAddr = "203.0.113.7:5555"
	req.SetBasicAuth(fixtureLocalPart, string(fixturePlainPassword))
	rec := httptest.NewRecorder()
	mw.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("bare username on a domainless nest must resolve by handle (Change A), got %d", rec.Code)
	}
	// validate_recipient fires once with the bare local-part and an EMPTY domain
	// — the signal that takes nest's handle→actor fallback (not a malformed
	// short-circuit).
	calls := caller.callsOf(wsrpc.MethodValidateRecipient)
	if len(calls) != 1 {
		t.Fatalf("validate_recipient fired %d times, want 1 (handle fallback)", len(calls))
	}
	var got map[string]any
	if err := cbor.Unmarshal(calls[0].body, &got); err != nil {
		t.Fatalf("decode validate_recipient body: %v", err)
	}
	if lp, _ := got["local_part"].(string); lp != fixtureLocalPart {
		t.Errorf("validate_recipient local_part = %q, want %q", lp, fixtureLocalPart)
	}
	if d, _ := got["domain"].(string); d != "" {
		t.Errorf("validate_recipient domain = %q, want \"\" (empty ⇒ handle fallback)", d)
	}
}
