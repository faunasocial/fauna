package davauth

import (
	"log/slog"
	"net/http"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// TestAuthMaterialCacheCollapsesRefetchWithinTTL is the headline TRACK-A
// fix: CalDAV is stateless HTTP Basic auth, so without an auth-material
// cache the MDA re-fetches the wrapped MLS blob (and the MLS snapshot)
// from nest on EVERY request. A burst then blows past nest's
// per-(bridge,actor,credential) `bridge_rate_limit` (30 events / 60 s,
// `bins/fauna-nest/src/bridge_rate_limit.rs`) → nest returns
// rate-limited on `fetch_wrapped_mls_blob` → the MDA 401s a legitimate
// request. The cache must collapse a request burst for one
// (actor, credential) to a SINGLE nest fetch within the TTL.
func TestAuthMaterialCacheCollapsesRefetchWithinTTL(t *testing.T) {
	caller := resolvableCaller(t)
	mw := NewMiddleware("fauna-caldav", okProbe(), caller, slog.Default(), nil, nil)

	const n = 6
	for i := 0; i < n; i++ {
		rec := caldavAuthReq(mw, string(fixturePlainPassword), "203.0.113.7")
		if rec.Code != http.StatusOK {
			t.Fatalf("request %d: good password must 200, got %d", i+1, rec.Code)
		}
	}

	// All N requests share ONE wrapped-blob fetch — the rest are cache hits.
	if got := len(caller.callsOf(wsrpc.MethodFetchWrappedMLSBlob)); got != 1 {
		t.Fatalf("fetch_wrapped_mls_blob fired %d times across %d requests, want 1 (cache must collapse the burst)", got, n)
	}
	if got := len(caller.callsOf(wsrpc.MethodValidateRecipient)); got != 1 {
		t.Fatalf("validate_recipient fired %d times across %d requests, want 1", got, n)
	}
	if got := len(caller.callsOf(wsrpc.MethodFetchMLSSnapshotBlob)); got != 1 {
		t.Fatalf("fetch_mls_snapshot_blob fired %d times across %d requests, want 1 (the snapshot bucket is rate-limited too)", got, n)
	}
}

// TestAuthMaterialCacheStillVerifiesPasswordPerRequest pins the security
// invariant the cache must NOT weaken (caldav-server.md § Architectural
// rules: "AEAD-success = AUTH-success"): the cache stores only the
// password-independent fetched material (ciphertext blob + public keys),
// never the unwrapped capability, so a cache hit STILL runs the
// per-request AEAD-unwrap of the presented password. A wrong password on
// a warm cache must therefore still 401 — and must NOT re-fetch the blob
// (the cached ciphertext is enough to fail the unwrap locally, which
// also keeps a brute-forcer from re-loading nest per attempt).
func TestAuthMaterialCacheStillVerifiesPasswordPerRequest(t *testing.T) {
	caller := resolvableCaller(t)
	mw := NewMiddleware("fauna-caldav", okProbe(), caller, slog.Default(), nil, nil)

	// Warm the cache with one good auth.
	if rec := caldavAuthReq(mw, string(fixturePlainPassword), "203.0.113.7"); rec.Code != http.StatusOK {
		t.Fatalf("warm-up good password must 200, got %d", rec.Code)
	}
	// Wrong password on the warm cache: still 401, no re-fetch.
	if rec := caldavAuthReq(mw, "WRONG-PASSWORD", "203.0.113.7"); rec.Code != http.StatusUnauthorized {
		t.Fatalf("wrong password on warm cache must 401, got %d", rec.Code)
	}
	if got := len(caller.callsOf(wsrpc.MethodFetchWrappedMLSBlob)); got != 1 {
		t.Fatalf("fetch_wrapped_mls_blob fired %d times, want 1 (wrong password must reuse the cached blob, not re-fetch)", got)
	}
	// A good password afterwards still works (cache intact, no poisoning).
	if rec := caldavAuthReq(mw, string(fixturePlainPassword), "203.0.113.7"); rec.Code != http.StatusOK {
		t.Fatalf("good password after a warm-cache failure must 200, got %d", rec.Code)
	}
}

// TestAuthMaterialCacheExpiresAfterTTL pins that the cache is genuinely
// time-bounded: once authMaterialTTL elapses, the next request re-fetches
// the material from nest (so a credential rotation propagates within the
// window rather than being pinned forever).
func TestAuthMaterialCacheExpiresAfterTTL(t *testing.T) {
	caller := resolvableCaller(t)
	mw := NewMiddleware("fauna-caldav", okProbe(), caller, slog.Default(), nil, nil).(*authMiddleware)

	base := time.Now()
	mw.now = func() time.Time { return base }
	if rec := caldavAuthReq(mw, string(fixturePlainPassword), "203.0.113.7"); rec.Code != http.StatusOK {
		t.Fatalf("first request must 200, got %d", rec.Code)
	}

	// Advance the clock just past the TTL → the entry is stale, so the
	// next request must re-fetch rather than serve the expired material.
	mw.now = func() time.Time { return base.Add(authMaterialTTL + time.Second) }
	if rec := caldavAuthReq(mw, string(fixturePlainPassword), "203.0.113.7"); rec.Code != http.StatusOK {
		t.Fatalf("post-TTL request must 200, got %d", rec.Code)
	}

	if got := len(caller.callsOf(wsrpc.MethodFetchWrappedMLSBlob)); got != 2 {
		t.Fatalf("fetch_wrapped_mls_blob fired %d times across two requests separated by > TTL, want 2 (TTL must expire the entry)", got)
	}
}
