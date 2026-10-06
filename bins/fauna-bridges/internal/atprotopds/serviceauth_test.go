package atprotopds

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoid"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// testSigner builds a real K-256 repo signing key from a fixed scalar, so the
// assertions below verify an ACTUAL signature rather than a fake's echo. The
// scalar is arbitrary but must be a valid one; PrivateKeyFromK256Scalar is the
// same constructor the production unseal path uses (projection.go).
func testSigner(t *testing.T) (RepoSigner, string) {
	t.Helper()
	scalar := make([]byte, 32)
	for i := range scalar {
		scalar[i] = byte(i + 1)
	}
	key, err := atprotoid.PrivateKeyFromK256Scalar(scalar)
	if err != nil {
		t.Fatalf("build test signing key: %v", err)
	}
	didKey, err := atprotoid.DIDKeyForPrivate(key)
	if err != nil {
		t.Fatalf("derive did:key: %v", err)
	}
	return key, didKey
}

// decodeJWT splits a compact JWT and returns its header, claims, signing input
// and raw signature — everything the assertions need to check the token as a
// verifier would, not as its own minter would.
func decodeJWT(t *testing.T, token string) (hdr, claims map[string]any, signingInput string, sig []byte) {
	t.Helper()
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		t.Fatalf("want a 3-part compact JWT, got %d parts: %q", len(parts), token)
	}
	decode := func(part, what string) map[string]any {
		raw, err := base64.RawURLEncoding.DecodeString(part)
		if err != nil {
			t.Fatalf("decode %s: %v", what, err)
		}
		var m map[string]any
		if err := json.Unmarshal(raw, &m); err != nil {
			t.Fatalf("unmarshal %s: %v", what, err)
		}
		return m
	}
	hdr = decode(parts[0], "header")
	claims = decode(parts[1], "claims")
	var err error
	sig, err = base64.RawURLEncoding.DecodeString(parts[2])
	if err != nil {
		t.Fatalf("decode signature: %v", err)
	}
	return hdr, claims, parts[0] + "." + parts[1], sig
}

// TestServiceAuthJWTVerifiesAgainstTheUsersRepoSigningKey is the load-bearing
// one: the minted token must verify against the ACCOUNT's own repo signing key
// (atproto-pds-full.md § Key material inventory — that key signs repo commits
// *and* service JWTs, which is what keeps C7's "no custody widening" true), and
// carry exactly the ratified claim set (`:199`).
func TestServiceAuthJWTVerifiesAgainstTheUsersRepoSigningKey(t *testing.T) {
	signer, didKey := testSigner(t)
	now := time.Unix(1_800_000_000, 0)

	token, err := MintServiceAuth(signer, ServiceAuthRequest{
		Iss: "did:fauna:alice",
		Aud: "did:web:api.bsky.app#bsky_appview",
		Lxm: "app.bsky.feed.getTimeline",
	}, now)
	if err != nil {
		t.Fatalf("mint: %v", err)
	}

	hdr, claims, signingInput, sig := decodeJWT(t, token)

	// The signature is the whole point — verify it the way a service would.
	pub, err := atprotoid.ParsePublicDIDKey(didKey)
	if err != nil {
		t.Fatalf("parse pubkey: %v", err)
	}
	if err := pub.HashAndVerify([]byte(signingInput), sig); err != nil {
		t.Fatalf("service JWT does not verify against the repo signing key: %v", err)
	}

	if hdr["alg"] != "ES256K" {
		t.Errorf("alg: want ES256K (the K-256 repo key's JWS alg), got %v", hdr["alg"])
	}
	if hdr["typ"] != "JWT" {
		t.Errorf("typ: want JWT, got %v", hdr["typ"])
	}
	if claims["iss"] != "did:fauna:alice" {
		t.Errorf("iss: want the user DID, got %v", claims["iss"])
	}
	if claims["aud"] != "did:web:api.bsky.app#bsky_appview" {
		t.Errorf("aud: want the target service DID, got %v", claims["aud"])
	}
	// The lxm binding is what stops a minted token being a bearer token for
	// every method the credential could reach.
	if claims["lxm"] != "app.bsky.feed.getTimeline" {
		t.Errorf("lxm: want the invoked method, got %v", claims["lxm"])
	}
	if jti, _ := claims["jti"].(string); jti == "" {
		t.Error("jti must be present and non-empty")
	}
	exp, ok := claims["exp"].(float64)
	if !ok {
		t.Fatalf("exp missing or not a number: %v", claims["exp"])
	}
	if want := float64(now.Add(MaxServiceAuthLifetime).Unix()); exp != want {
		t.Errorf("exp: want %v (now + the 60s cap), got %v", want, exp)
	}
}

// TestServiceAuthLifetimeIsCappedAtSixtySeconds pins `exp ≤ 60s` (`:199`)
// against a caller asking for longer. A client-chosen lifetime may only ever
// shorten the token, never extend it.
func TestServiceAuthLifetimeIsCappedAtSixtySeconds(t *testing.T) {
	signer, _ := testSigner(t)
	now := time.Unix(1_800_000_000, 0)

	for _, tc := range []struct {
		name     string
		lifetime time.Duration
		wantExp  int64
	}{
		{"asked for an hour", time.Hour, now.Add(MaxServiceAuthLifetime).Unix()},
		{"asked for exactly the cap", MaxServiceAuthLifetime, now.Add(MaxServiceAuthLifetime).Unix()},
		{"asked for less", 10 * time.Second, now.Add(10 * time.Second).Unix()},
		{"asked for nothing (the default)", 0, now.Add(MaxServiceAuthLifetime).Unix()},
		{"asked for a negative lifetime", -time.Hour, now.Add(MaxServiceAuthLifetime).Unix()},
	} {
		t.Run(tc.name, func(t *testing.T) {
			token, err := MintServiceAuth(signer, ServiceAuthRequest{
				Iss:      "did:fauna:alice",
				Aud:      "did:web:api.bsky.app",
				Lxm:      "app.bsky.feed.getTimeline",
				Lifetime: tc.lifetime,
			}, now)
			if err != nil {
				t.Fatalf("mint: %v", err)
			}
			_, claims, _, _ := decodeJWT(t, token)
			if got := int64(claims["exp"].(float64)); got != tc.wantExp {
				t.Errorf("exp: want %d, got %d", tc.wantExp, got)
			}
		})
	}
}

// TestServiceAuthJtiIsUniquePerMint — `jti` unique (`:199`). A repeated jti
// would let a replay-detecting service reject the second legitimate call, and
// would defeat replay detection where it is enforced.
func TestServiceAuthJtiIsUniquePerMint(t *testing.T) {
	signer, _ := testSigner(t)
	now := time.Unix(1_800_000_000, 0)
	req := ServiceAuthRequest{Iss: "did:fauna:alice", Aud: "did:web:api.bsky.app", Lxm: "app.bsky.feed.getTimeline"}

	seen := map[string]bool{}
	for i := 0; i < 50; i++ {
		token, err := MintServiceAuth(signer, req, now)
		if err != nil {
			t.Fatalf("mint %d: %v", i, err)
		}
		_, claims, _, _ := decodeJWT(t, token)
		jti := claims["jti"].(string)
		if seen[jti] {
			t.Fatalf("jti %q repeated at mint %d — same inputs, same clock", jti, i)
		}
		seen[jti] = true
	}
}

// TestMintServiceAuthRefusesIncompleteClaims — the mint refuses rather than
// emitting a token with an empty binding. An empty `lxm` or `aud` is a token
// good for anything, anywhere; that must be unrepresentable, not merely
// unrequested.
func TestMintServiceAuthRefusesIncompleteClaims(t *testing.T) {
	signer, _ := testSigner(t)
	now := time.Unix(1_800_000_000, 0)
	full := ServiceAuthRequest{Iss: "did:fauna:alice", Aud: "did:web:api.bsky.app", Lxm: "app.bsky.feed.getTimeline"}

	for _, tc := range []struct {
		name  string
		mutch func(*ServiceAuthRequest)
	}{
		{"no iss", func(r *ServiceAuthRequest) { r.Iss = "" }},
		{"no aud", func(r *ServiceAuthRequest) { r.Aud = "" }},
		{"no lxm", func(r *ServiceAuthRequest) { r.Lxm = "" }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			req := full
			tc.mutch(&req)
			if _, err := MintServiceAuth(signer, req, now); err == nil {
				t.Fatal("want a refusal, got a token")
			}
		})
	}
}

// ── the getServiceAuth route ────────────────────────────────────────────────

// stubSigners is the RepoSignerSource seam under test: production unseals the
// per-user blob (cgo), tests hand back a fixed key.
type stubSigners struct {
	signer RepoSigner
	err    error
	calls  int
}

func (s *stubSigners) RepoSigner(context.Context, []byte) (RepoSigner, error) {
	s.calls++
	if s.err != nil {
		return nil, s.err
	}
	return s.signer, nil
}

// serviceAuthFixture drives getServiceAuth through the REAL route table and
// middleware chain, so the frame's own authz slot runs ahead of the handler —
// the two-check property (`:193`) is only meaningful end-to-end.
type serviceAuthFixture struct {
	authz   *stubAuthorizer
	signers *stubSigners
	http    *httptest.Server
}

func newServiceAuthFixture(t *testing.T) *serviceAuthFixture {
	t.Helper()
	signer, _ := testSigner(t)
	authz := &stubAuthorizer{}
	signers := &stubSigners{signer: signer}
	srv := NewServer(nil, nil, authz, signers, nil)
	srv.flags.set(make([]byte, 32), true) // kill-switch on, so the default stub allows

	caller := &xrpc.Caller{
		ActorID: make([]byte, 32),
		DID:     "did:fauna:alice",
		Handle:  "alice",
		Scope:   ScopeAppPass,
		Plane:   PlaneAppCredential,
	}
	x := xrpc.NewServer(staticCaller{caller}, nil, srv.AuthzHook, nil, nil)
	srv.RegisterRoutes(x)
	ts := httptest.NewServer(x)
	t.Cleanup(ts.Close)
	return &serviceAuthFixture{authz: authz, signers: signers, http: ts}
}

func (f *serviceAuthFixture) get(t *testing.T, query string) (int, map[string]any) {
	t.Helper()
	req, err := http.NewRequest(http.MethodGet, f.http.URL+"/xrpc/com.atproto.server.getServiceAuth?"+query, nil)
	if err != nil {
		t.Fatalf("build request: %v", err)
	}
	req.Header.Set("Authorization", "Bearer token")
	resp, err := f.http.Client().Do(req)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	defer resp.Body.Close()
	var body map[string]any
	_ = json.NewDecoder(resp.Body).Decode(&body)
	return resp.StatusCode, body
}

// TestGetServiceAuthMintsForAnAuthorizedRequest — the happy path, end to end
// through the frame.
func TestGetServiceAuthMintsForAnAuthorizedRequest(t *testing.T) {
	f := newServiceAuthFixture(t)
	status, body := f.get(t, "aud=did:web:api.bsky.app%23bsky_appview&lxm=app.bsky.feed.getTimeline")
	if status != http.StatusOK {
		t.Fatalf("want 200, got %d (%v)", status, body)
	}
	token, _ := body["token"].(string)
	if token == "" {
		t.Fatalf("want a token in the response, got %v", body)
	}
	_, claims, _, _ := decodeJWT(t, token)
	if claims["aud"] != "did:web:api.bsky.app#bsky_appview" {
		t.Errorf("aud: want the requested audience, got %v", claims["aud"])
	}
	if claims["lxm"] != "app.bsky.feed.getTimeline" {
		t.Errorf("lxm: want the requested method, got %v", claims["lxm"])
	}
	// iss is the CALLER's DID, never a client-supplied one.
	if claims["iss"] != "did:fauna:alice" {
		t.Errorf("iss: want the authenticated caller's DID, got %v", claims["iss"])
	}
}

// TestGetServiceAuthRunsTheSecondD8Check is forward-watch (i) from the phase-3
// security review: the handler MUST consult Server.AuthorizeServiceAuth with
// the REQUESTED lxm/aud. Without it, minting silently bypasses D8's second
// check — the route hook alone only asked "may this credential mint at all".
func TestGetServiceAuthRunsTheSecondD8Check(t *testing.T) {
	f := newServiceAuthFixture(t)

	var sawSecond bool
	f.authz.decide = func(in AuthzInput) AuthzVerdict {
		// The second check is the one carrying the *requested* method, not the
		// route's own NSID.
		if in.Lxm == "com.atproto.repo.importRepo" {
			sawSecond = true
			return AuthzVerdict{XrpcError: "MethodNotImplemented", Message: "account migration is not yet supported"}
		}
		return AuthzVerdict{Allow: true}
	}

	status, body := f.get(t, "aud=did:web:pds.example.com&lxm=com.atproto.repo.importRepo")
	if !sawSecond {
		t.Fatal("the handler never asked D8 about the REQUESTED lxm — the second check is missing")
	}
	if status == http.StatusOK {
		t.Fatalf("a deferred-refused lxm must not mint, got 200 (%v)", body)
	}
	if body["error"] != "MethodNotImplemented" {
		t.Errorf("want the module's error name verbatim, got %v", body["error"])
	}
	if _, minted := body["token"]; minted {
		t.Error("a refused request must not carry a token")
	}
	// A refusal must never reach the signing key at all.
	if f.signers.calls != 0 {
		t.Errorf("a refused mint unsealed the signing key %d time(s) — refuse before touching key material", f.signers.calls)
	}
}

// TestGetServiceAuthRequiresBothParameters — an unbound token must be
// impossible to request, not merely discouraged.
func TestGetServiceAuthRequiresBothParameters(t *testing.T) {
	for _, tc := range []struct{ name, query string }{
		{"no lxm", "aud=did:web:api.bsky.app"},
		{"no aud", "lxm=app.bsky.feed.getTimeline"},
		{"neither", ""},
		{"blank lxm", "aud=did:web:api.bsky.app&lxm=%20"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := newServiceAuthFixture(t)
			status, body := f.get(t, tc.query)
			if status != http.StatusBadRequest {
				t.Fatalf("want 400, got %d (%v)", status, body)
			}
			if f.signers.calls != 0 {
				t.Errorf("an invalid request unsealed the signing key %d time(s)", f.signers.calls)
			}
			if f.authz.count() > 1 {
				t.Errorf("an invalid request ran the D8 mint check %d times — validate before authorizing", f.authz.count()-1)
			}
		})
	}
}

// TestGetServiceAuthFailsClosedWithoutASignerSource — the closed-world rule
// extends to the key seam: no signer, no token, and nothing that reads like a
// success.
func TestGetServiceAuthFailsClosedWithoutASignerSource(t *testing.T) {
	f := newServiceAuthFixture(t)
	f.signers.err = errors.New("unseal failed")

	status, body := f.get(t, "aud=did:web:api.bsky.app&lxm=app.bsky.feed.getTimeline")
	if status != http.StatusInternalServerError {
		t.Fatalf("want 500, got %d (%v)", status, body)
	}
	if _, minted := body["token"]; minted {
		t.Error("a failed unseal must not carry a token")
	}
	// The reason must not leak to the caller — an unseal failure is our
	// problem, and its detail describes bridge-internal key state.
	if msg, _ := body["message"].(string); strings.Contains(msg, "unseal failed") {
		t.Errorf("the internal error leaked to the wire: %q", msg)
	}
}

// TestGetServiceAuthIgnoresAForgedProxyHeader — `getServiceAuth` mints for a
// client; it is not itself a call that may be redirected elsewhere, so it must
// not be Proxyable. If it were, an attacker-supplied `atproto-proxy` header
// would become the audience D8 sees on the route check while the handler minted
// for the query's `aud` — two different audiences from one request, which is
// exactly the skew Proxyable exists to prevent. Asserted through the wire
// rather than by reading the route table, so it covers what actually reaches
// the decision.
func TestGetServiceAuthIgnoresAForgedProxyHeader(t *testing.T) {
	f := newServiceAuthFixture(t)

	var inputs []AuthzInput
	f.authz.decide = func(in AuthzInput) AuthzVerdict {
		inputs = append(inputs, in)
		return AuthzVerdict{Allow: true}
	}

	req, err := http.NewRequest(http.MethodGet,
		f.http.URL+"/xrpc/com.atproto.server.getServiceAuth?aud=did:web:api.bsky.app&lxm=app.bsky.feed.getTimeline", nil)
	if err != nil {
		t.Fatalf("build request: %v", err)
	}
	req.Header.Set("Authorization", "Bearer token")
	req.Header.Set("atproto-proxy", "did:web:evil.example#attacker")
	resp, err := f.http.Client().Do(req)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	defer resp.Body.Close()

	if len(inputs) != 2 {
		t.Fatalf("want exactly 2 D8 calls (route check, then mint check), got %d: %+v", len(inputs), inputs)
	}
	// Call 1 — the frame's route check. The forged header must not reach it.
	if inputs[0].Lxm != "com.atproto.server.getServiceAuth" {
		t.Errorf("first check: want the route NSID, got %q", inputs[0].Lxm)
	}
	if inputs[0].Aud != nil {
		t.Errorf("the forged atproto-proxy header reached the route check as aud=%q — the route must not be Proxyable", *inputs[0].Aud)
	}
	// Call 2 — the mint check. Its audience is the QUERY's, never the header's.
	if inputs[1].Lxm != "app.bsky.feed.getTimeline" {
		t.Errorf("second check: want the requested lxm, got %q", inputs[1].Lxm)
	}
	if inputs[1].Aud == nil || *inputs[1].Aud != "did:web:api.bsky.app" {
		t.Errorf("second check: want the query's aud, got %v", inputs[1].Aud)
	}
}

// TestGetServiceAuthRefusesAnUnauthenticatedCall — the route mints with the
// caller's own key, so it must never be Public.
func TestGetServiceAuthRefusesAnUnauthenticatedCall(t *testing.T) {
	signer, _ := testSigner(t)
	signers := &stubSigners{signer: signer}
	srv := NewServer(nil, nil, &stubAuthorizer{}, signers, nil)
	// No caller resolves — an anonymous request.
	x := xrpc.NewServer(nil, nil, srv.AuthzHook, nil, nil)
	srv.RegisterRoutes(x)
	ts := httptest.NewServer(x)
	t.Cleanup(ts.Close)

	resp, err := ts.Client().Get(ts.URL + "/xrpc/com.atproto.server.getServiceAuth?aud=did:web:api.bsky.app&lxm=app.bsky.feed.getTimeline")
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("want 401 for an anonymous mint request, got %d", resp.StatusCode)
	}
	if signers.calls != 0 {
		t.Errorf("an anonymous request reached the signing key %d time(s)", signers.calls)
	}
}
