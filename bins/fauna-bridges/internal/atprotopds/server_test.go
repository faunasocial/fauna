package atprotopds

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"

	"github.com/fxamacker/cbor/v2"
	"golang.org/x/crypto/argon2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// lightPHC mints a cheap-parameter Argon2id PHC string (production strings
// verify under their own embedded params, so the handler path is identical).
func lightPHC(secret string) string {
	salt := []byte("0123456789abcdef")
	hash := argon2.IDKey([]byte(secret), salt, 1, 4096, 1, 32)
	enc := base64.RawStdEncoding.EncodeToString
	return "$argon2id$v=19$m=4096,t=1,p=1$" + enc(salt) + "$" + enc(hash)
}

// testLoginDID is the fixture account's real ATProto DID. Every test that
// needs "the account's DID" uses this one value, so a handler that answers
// anything else is visibly re-deriving rather than carrying.
const testLoginDID = "did:plc:7iza6de2dwap2sbkpav7c6c6"

// testActorID is the fixture account's 32-byte Fauna actor id — the value the
// `fauna_actor` claim carries, and the one every nest-facing call is made with.
var testActorID = bytes.Repeat([]byte{0x42}, 32)

// fakeNest implements wsrpc.Caller with nest-faithful F1 registry
// semantics: verifier rows, the kill-switch flag, and rotate-on-use
// sessions with one-row reuse detection. Bodies are round-tripped through
// dagcbor into local mirrors, so the wire tags of the real request structs
// are exercised too.
type fakeNest struct {
	mu sync.Mutex

	actorID   []byte
	handle    string
	enabled   bool
	verifiers []wsrpc.AppCredentialVerifier
	// loginDID is the account's real ATProto DID as the nest reports it —
	// `login_did`, filled only for an ACTIVE identity with a minted DID (slice
	// 4d). Empty = no active hosted identity, which the bridge must treat as a
	// uniform auth failure rather than inventing a DID of its own.
	loginDID string

	// sessions: family id (hex-ish string of session_id bytes) → state.
	sessions map[string]*fakeSession

	// preferences: the opaque putPreferences payload (nil until first store),
	// stored and returned verbatim like the real nest kind.
	preferences []byte

	// ingest answers fauna.bridges.atproto.ingest_external_write. Wired
	// per-test (repo_write_test.go) so a test states exactly what the nest
	// decided — round-trip, journal, or a sub-typed refusal.
	ingest          func([]wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult
	ingestCalls     int
	lastIngest      []wsrpc.ExternalWrite
	lastIngestActor []byte

	recordCalls    int
	endCalls       int
	storePrefCalls int
}

type fakeSession struct {
	currentJti []byte
	revoked    bool
}

func (f *fakeNest) Call(_ context.Context, method string, body any, reply any) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	switch method {
	case wsrpc.MethodAtprotoFetchAppCredentialVerifiers:
		var req struct {
			Identifier string `cbor:"identifier"`
		}
		fakeReencode(body, &req)
		out := wsrpc.AppCredentialVerifiers{ExternalAppsEnabled: true}
		if req.Identifier == f.handle || (f.loginDID != "" && req.Identifier == f.loginDID) {
			out.ActorID = f.actorID
			out.ExternalAppsEnabled = f.enabled
			out.Verifiers = f.verifiers
			if f.loginDID != "" {
				d := f.loginDID
				out.LoginDID = &d
			}
		}
		fakeReencode(out, reply)
		return nil
	case wsrpc.MethodAtprotoRecordSession:
		var req struct {
			ActorID   []byte `cbor:"actor_id"`
			SessionID []byte `cbor:"session_id"`
			Plane     string `cbor:"plane"`
			ExpiresAt int64  `cbor:"expires_at"`
		}
		fakeReencode(body, &req)
		if !f.enabled {
			return fmt.Errorf("nest: fauna.bridges.atproto.disabled")
		}
		f.recordCalls++
		f.sessions[string(req.SessionID)] = &fakeSession{currentJti: req.SessionID}
		fakeReencode(map[string]any{"ok": true}, reply)
		return nil
	case wsrpc.MethodAtprotoRefreshSession:
		var req struct {
			SessionID    []byte `cbor:"session_id"`
			PresentedJti []byte `cbor:"presented_jti"`
			NewJti       []byte `cbor:"new_jti"`
		}
		fakeReencode(body, &req)
		status := wsrpc.RefreshStatusInvalid
		if !f.enabled {
			status = wsrpc.RefreshStatusDisabled
		} else if s, ok := f.sessions[string(req.SessionID)]; ok && !s.revoked {
			if bytes.Equal(s.currentJti, req.PresentedJti) {
				s.currentJti = req.NewJti
				status = wsrpc.RefreshStatusRotated
			} else {
				s.revoked = true
				status = wsrpc.RefreshStatusReuseDetected
			}
		}
		fakeReencode(map[string]any{"status": status}, reply)
		return nil
	case wsrpc.MethodAtprotoEndSession:
		var req struct {
			SessionID []byte `cbor:"session_id"`
		}
		fakeReencode(body, &req)
		f.endCalls++
		s, ok := f.sessions[string(req.SessionID)]
		ended := ok && !s.revoked
		if ok {
			s.revoked = true
		}
		fakeReencode(map[string]any{"ended": ended}, reply)
		return nil
	case wsrpc.MethodAtprotoFetchPreferences:
		// None → CBOR null (the real Option<ByteBuf> shape), so the handler's
		// empty-array default path is exercised.
		out := map[string]any{"preferences": nil}
		if f.preferences != nil {
			out["preferences"] = f.preferences
		}
		fakeReencode(out, reply)
		return nil
	case wsrpc.MethodAtprotoStorePreferences:
		var req struct {
			Preferences []byte `cbor:"preferences"`
		}
		fakeReencode(body, &req)
		f.storePrefCalls++
		f.preferences = req.Preferences
		fakeReencode(map[string]any{"ok": true}, reply)
		return nil
	case wsrpc.MethodAtprotoIngestExternalWrite:
		var req struct {
			ActorID []byte                `cbor:"actor_id"`
			Writes  []wsrpc.ExternalWrite `cbor:"writes"`
		}
		fakeReencode(body, &req)
		f.ingestCalls++
		f.lastIngestActor = req.ActorID
		f.lastIngest = req.Writes
		if f.ingest == nil {
			return fmt.Errorf("fakeNest: no ingest_external_write behavior wired")
		}
		fakeReencode(map[string]any{"results": f.ingest(req.Writes)}, reply)
		return nil
	default:
		return fmt.Errorf("fakeNest: unexpected method %s", method)
	}
}

// fakeReencode round-trips a value through canonical DAG-CBOR the same way
// the real wsrpc client does (dagcbor.Marshal out, cbor.Unmarshal into the
// caller's pointer), so the request structs' wire tags are exercised. The
// fake panics on marshal bugs, which the test surfaces just as loudly.
func fakeReencode(body, into any) {
	raw, err := dagcbor.Marshal(body)
	if err != nil {
		panic(err)
	}
	if err := cbor.Unmarshal(raw, into); err != nil {
		panic(err)
	}
}

type fixture struct {
	nest   *fakeNest
	server *Server
	http   *httptest.Server
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	actor := testActorID
	nest := &fakeNest{
		actorID: actor,
		handle:  "alice",
		enabled: true,
		// A real did:plc, as the nest reports it post-4d. Deliberately NOT
		// derivable from actorID: any handler that reproduces this string can
		// only have carried the nest's value.
		loginDID: testLoginDID,
		verifiers: []wsrpc.AppCredentialVerifier{
			{CredentialID: "ivory", Verifier: lightPHC("3ssn-cuqp-4u7r-farx"), DmAllowed: false},
		},
		sessions: make(map[string]*fakeSession),
	}
	minter := NewTokenMinter(StaticSecret("0123456789abcdef0123456789abcdef"), testServiceDID, nil)
	// The D8 stub honours the kill-switch and allows otherwise — see
	// stubAuthorizer's doc comment in authz_test.go for why it does not
	// reimplement the matrix.
	srv := NewServer(nest, minter, &stubAuthorizer{}, nil, nil)
	x := xrpc.NewServer(srv, nil, srv.AuthzHook, xrpc.NewIPLimiter(nil), nil)
	srv.RegisterRoutes(x)
	ts := httptest.NewServer(x)
	t.Cleanup(ts.Close)
	return &fixture{nest: nest, server: srv, http: ts}
}

type sessionReply struct {
	AccessJwt  string `json:"accessJwt"`
	RefreshJwt string `json:"refreshJwt"`
	Handle     string `json:"handle"`
	Did        string `json:"did"`
}

func (f *fixture) createSession(t *testing.T, identifier, password string) (*http.Response, *sessionReply) {
	t.Helper()
	body, _ := json.Marshal(map[string]string{"identifier": identifier, "password": password})
	resp, err := http.Post(f.http.URL+"/xrpc/com.atproto.server.createSession", "application/json", bytes.NewReader(body))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { resp.Body.Close() })
	if resp.StatusCode != http.StatusOK {
		return resp, nil
	}
	var out sessionReply
	if err := json.NewDecoder(resp.Body).Decode(&out); err != nil {
		t.Fatal(err)
	}
	return resp, &out
}

func (f *fixture) get(t *testing.T, nsid, bearer string) *http.Response {
	t.Helper()
	req, _ := http.NewRequest(http.MethodGet, f.http.URL+"/xrpc/"+nsid, nil)
	if bearer != "" {
		req.Header.Set("Authorization", "Bearer "+bearer)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { resp.Body.Close() })
	return resp
}

func (f *fixture) post(t *testing.T, nsid, bearer string) *http.Response {
	t.Helper()
	req, _ := http.NewRequest(http.MethodPost, f.http.URL+"/xrpc/"+nsid, nil)
	if bearer != "" {
		req.Header.Set("Authorization", "Bearer "+bearer)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { resp.Body.Close() })
	return resp
}

func errName(t *testing.T, resp *http.Response) string {
	t.Helper()
	var e struct {
		Error string `json:"error"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&e); err != nil {
		t.Fatalf("decode error body: %v", err)
	}
	return e.Error
}

func TestCreateSessionHappyPathAndGetSession(t *testing.T) {
	f := newFixture(t)
	resp, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("createSession status %d", resp.StatusCode)
	}
	// The session names the account's REAL DID, straight from the nest's
	// login_did — never a placeholder the bridge computed (slice 4d).
	if sess.Handle != "alice" || sess.Did != testLoginDID {
		t.Fatalf("reply identity: %+v", sess)
	}
	if f.nest.recordCalls != 1 {
		t.Fatalf("record_session calls: %d", f.nest.recordCalls)
	}

	// The authed no-op: getSession with the minted access token.
	got := f.get(t, "com.atproto.server.getSession", sess.AccessJwt)
	if got.StatusCode != http.StatusOK {
		t.Fatalf("getSession status %d", got.StatusCode)
	}
	var body map[string]any
	_ = json.NewDecoder(got.Body).Decode(&body)
	if body["handle"] != "alice" || body["did"] != sess.Did {
		t.Fatalf("getSession body: %v", body)
	}

	// A refresh token is NOT an access token.
	if s := f.get(t, "com.atproto.server.getSession", sess.RefreshJwt).StatusCode; s != http.StatusUnauthorized {
		t.Fatalf("getSession with refresh token: %d", s)
	}
	// Garbage and missing tokens refuse uniformly.
	if s := f.get(t, "com.atproto.server.getSession", "garbage").StatusCode; s != http.StatusUnauthorized {
		t.Fatalf("getSession with garbage: %d", s)
	}
	if s := f.get(t, "com.atproto.server.getSession", "").StatusCode; s != http.StatusUnauthorized {
		t.Fatalf("getSession without token: %d", s)
	}
}

func TestCreateSessionUniformFailures(t *testing.T) {
	f := newFixture(t)
	// Wrong password and unknown identifier: identical status AND error name.
	respWrong, _ := f.createSession(t, "alice", "aaaa-bbbb-cccc-dddd")
	respUnknown, _ := f.createSession(t, "nobody", "aaaa-bbbb-cccc-dddd")
	if respWrong.StatusCode != http.StatusUnauthorized || respUnknown.StatusCode != http.StatusUnauthorized {
		t.Fatalf("statuses: %d %d", respWrong.StatusCode, respUnknown.StatusCode)
	}
	nameWrong, nameUnknown := errName(t, respWrong), errName(t, respUnknown)
	if nameWrong != "AuthenticationRequired" || nameWrong != nameUnknown {
		t.Fatalf("error names differ: %q vs %q", nameWrong, nameUnknown)
	}
	if f.nest.recordCalls != 0 {
		t.Fatal("a failed login recorded a session")
	}
}

func TestCreateSessionKillSwitchOffIsUniform(t *testing.T) {
	f := newFixture(t)
	f.nest.enabled = false
	resp, _ := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	if resp.StatusCode != http.StatusUnauthorized || errName(t, resp) != "AuthenticationRequired" {
		t.Fatalf("disabled account not uniform: %d", resp.StatusCode)
	}
}

func TestPerIdentifierLockout(t *testing.T) {
	f := newFixture(t)
	var last *http.Response
	for i := 0; i < lockoutLimit+1; i++ {
		last, _ = f.createSession(t, "alice", "aaaa-bbbb-cccc-dddd")
	}
	// The lockout trips before the per-IP ClassAuth window (10/5min) only
	// via the coarse buckets; at the fine bucket both layers refuse — the
	// last attempt must be a 429 from one of them.
	if last.StatusCode != http.StatusTooManyRequests {
		t.Fatalf("no lockout after %d failures: %d", lockoutLimit+1, last.StatusCode)
	}
}

func TestKillSwitchNudgeCutsLiveTokens(t *testing.T) {
	f := newFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	if s := f.get(t, "com.atproto.server.getSession", sess.AccessJwt).StatusCode; s != http.StatusOK {
		t.Fatalf("pre-flip getSession: %d", s)
	}
	off := false
	f.server.HandleSessionsChanged(f.nest.actorID, &off)
	if s := f.get(t, "com.atproto.server.getSession", sess.AccessJwt).StatusCode; s != http.StatusUnauthorized {
		t.Fatal("kill-switch OFF did not cut a live access token")
	}
	on := true
	f.server.HandleSessionsChanged(f.nest.actorID, &on)
	if s := f.get(t, "com.atproto.server.getSession", sess.AccessJwt).StatusCode; s != http.StatusOK {
		t.Fatal("kill-switch ON did not restore the token")
	}
}

func TestRefreshRotationAndReuseDetection(t *testing.T) {
	f := newFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")

	// Rotate once.
	r1 := f.post(t, "com.atproto.server.refreshSession", sess.RefreshJwt)
	if r1.StatusCode != http.StatusOK {
		t.Fatalf("first refresh: %d", r1.StatusCode)
	}
	var rotated sessionReply
	_ = json.NewDecoder(r1.Body).Decode(&rotated)
	if rotated.RefreshJwt == sess.RefreshJwt {
		t.Fatal("refresh did not rotate the refresh token")
	}
	if _, err := f.server.minter.VerifyAccessScope(rotated.AccessJwt); err != nil {
		t.Fatalf("rotated access token invalid: %v", err)
	}

	// Replay the ORIGINAL refresh token → reuse detected → family dead.
	r2 := f.post(t, "com.atproto.server.refreshSession", sess.RefreshJwt)
	if r2.StatusCode != http.StatusUnauthorized {
		t.Fatalf("replayed refresh accepted: %d", r2.StatusCode)
	}
	// Even the CURRENT rotated token now refuses (family killed).
	r3 := f.post(t, "com.atproto.server.refreshSession", rotated.RefreshJwt)
	if r3.StatusCode != http.StatusUnauthorized {
		t.Fatalf("family survived reuse detection: %d", r3.StatusCode)
	}
}

func TestDeleteSession(t *testing.T) {
	f := newFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	if s := f.post(t, "com.atproto.server.deleteSession", sess.RefreshJwt).StatusCode; s != http.StatusOK {
		t.Fatalf("deleteSession: %d", s)
	}
	if f.nest.endCalls != 1 {
		t.Fatalf("end_session calls: %d", f.nest.endCalls)
	}
	// The revoked family refuses refresh.
	if s := f.post(t, "com.atproto.server.refreshSession", sess.RefreshJwt).StatusCode; s != http.StatusUnauthorized {
		t.Fatal("refresh after deleteSession succeeded")
	}
}

func TestDescribeServerStatic(t *testing.T) {
	f := newFixture(t)
	resp := f.get(t, "com.atproto.server.describeServer", "")
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("describeServer: %d", resp.StatusCode)
	}
	var body struct {
		Did                  string   `json:"did"`
		AvailableUserDomains []string `json:"availableUserDomains"`
	}
	_ = json.NewDecoder(resp.Body).Decode(&body)
	if body.Did != testServiceDID || len(body.AvailableUserDomains) != 0 {
		t.Fatalf("describeServer body: %+v", body)
	}
}

func TestFrameRouting(t *testing.T) {
	f := newFixture(t)
	// Unknown NSID.
	resp := f.get(t, "com.atproto.server.unknownMethod", "")
	if resp.StatusCode != http.StatusNotFound || errName(t, resp) != "MethodNotImplemented" {
		t.Fatalf("unknown NSID: %d", resp.StatusCode)
	}
	// Wrong HTTP method for a procedure.
	resp = f.get(t, "com.atproto.server.createSession", "")
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("GET on a procedure: %d", resp.StatusCode)
	}
}

// An account with no ACTIVE hosted ATProto identity cannot open a session —
// and fails in the same uniform way as every other login refusal, so the
// XRPC client learns nothing about which of them applied (slice 4d).
//
// This is not only the "not enabled yet" case. `atproto-pds-bridge.md`
// § Disable & revocation ratifies that stepping down off a hosted level
// suspends the account's ENTIRE ATProto presence, live app sessions included.
// The nest revoked those sessions already; before 4d nothing stopped the app
// simply calling createSession again on its next request, because the login
// path never consulted identity status and the user's own kill-switch (a
// separate, user-owned toggle) is untouched by a step-down. `login_did` going
// nil is what makes the ratified suspension actually hold.
func TestCreateSessionRefusesWithoutAnActiveHostedIdentity(t *testing.T) {
	f := newFixture(t)
	f.nest.loginDID = "" // the nest reports no active identity with a minted DID

	resp, _ := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	if resp.StatusCode == http.StatusOK {
		t.Fatal("createSession succeeded for an account with no active hosted identity; " +
			"a step-down would be undone by the very next login")
	}
	// Uniform with a wrong secret / unknown identifier / kill-switch OFF: same
	// name, so no enumeration signal leaves the box.
	if got := errName(t, resp); got != "AuthenticationRequired" {
		t.Fatalf("error name = %q, which distinguishes this case from a wrong secret", got)
	}
	// And nothing was registered nest-side for a session that must not exist.
	if f.nest.recordCalls != 0 {
		t.Fatalf("record_session calls = %d", f.nest.recordCalls)
	}
}

// The actor id travels in its own claim, not inside the DID. A token without
// it is refused rather than defaulted (a malformed or forged token fails
// closed), so the bridge never hex-decodes a `did:fauna:<hex>` `sub` back into
// an actor id.
func TestAccessTokenWithoutTheActorClaimIsRefused(t *testing.T) {
	f := newFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")

	// Sanity: the minted token carries the account's actor id.
	claims, err := f.server.minter.VerifyAccessScope(sess.AccessJwt)
	if err != nil {
		t.Fatal(err)
	}
	if claims.FaunaActor != hex.EncodeToString(testActorID) {
		t.Fatalf("fauna_actor = %q", claims.FaunaActor)
	}
	if claims.Sub != testLoginDID {
		t.Fatalf("sub = %q, not the account's real DID", claims.Sub)
	}

	// A token with a real signature and every other claim but the actor must
	// not authenticate anything.
	stripped, err := f.server.minter.sign(Claims{
		Sub: testLoginDID, Aud: testServiceDID, Scope: ScopeAppPass,
		Iat: claims.Iat, Exp: claims.Exp, Jti: claims.Jti, Sid: claims.Sid, Handle: "alice",
		Plane: PlaneAppCredential,
	})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := f.server.VerifyAccess(nil, stripped); err == nil {
		t.Fatal("a token with no fauna_actor claim authenticated; the actor id would have to be guessed")
	}
	if got := f.get(t, "com.atproto.server.getSession", stripped); got.StatusCode == http.StatusOK {
		t.Fatalf("getSession accepted an actor-less token: %d", got.StatusCode)
	}
}
