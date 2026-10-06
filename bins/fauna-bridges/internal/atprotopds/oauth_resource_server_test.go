package atprotopds

// The resource-server plane: a nest-minted, DPoP-bound access token presented
// on an XRPC call. What is faked is the shared-Rust DPoP module (its own rules
// are tested in Rust and pinned cross-binary in cmd/fauna-atproto-bridge) and
// the nest's authorization server, stood in for by a real P-256 key that signs
// tokens the way the nest's does.

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// ── Fixtures ─────────────────────────────────────────────────────────────────

// testClock is a hand-advanced clock. Every TTL assertion moves it explicitly
// rather than sleeping — testing.md convention 14: assert latency-independent
// state, never wall-clock timing.
type testClock struct {
	mu sync.Mutex
	t  time.Time
}

func newTestClock() *testClock {
	return &testClock{t: time.Date(2026, 7, 31, 12, 0, 0, 0, time.UTC)}
}
func (c *testClock) now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.t
}
func (c *testClock) advance(d time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.t = c.t.Add(d)
}

// fakeDPoPPolicy stands in for `fauna_bridge_atproto::dpop` with
// [decodeProofLikeTheModuleDoes], and records the expectations it was handed —
// which is what the Go half owns on this path.
type fakeDPoPPolicy struct {
	mu     sync.Mutex
	n      int
	expect DPoPExpectations
}

func (f *fakeDPoPPolicy) ValidateDPoPProof(compact string, expect DPoPExpectations) DPoPVerdict {
	f.mu.Lock()
	f.n++
	f.expect = expect
	f.mu.Unlock()
	return decodeProofLikeTheModuleDoes(compact)
}

func (f *fakeDPoPPolicy) calls() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.n
}

func (f *fakeDPoPPolicy) dpopExpect() DPoPExpectations {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.expect
}

// testPDSOrigin is what production takes from
// `fauna_bridge_atproto::oauth_metadata::oauth_issuer` over the PDS host — the
// prefix every resource-server proof's `htu` is built from.
const testPDSOrigin = "https://pds.example.com"

// nestIssuerURL is the identifier the nest-hosted authorization server mints
// under — the apex, never the PDS host.
const nestIssuerURL = "https://example.com"

// rsServer builds a Server with the resource-server plane wired, the app-plane
// minter present (its service DID is the `aud` access tokens must name), and a
// hand-advanced clock. No issuer is fed: each test decides that.
func rsServer(t *testing.T) (*Server, *fakeDPoPPolicy, *testClock) {
	t.Helper()
	clock := newTestClock()
	s := NewServer(nil, testMinter(t, clock.now), nil, nil, nil)
	// Set directly rather than through a setter: these tests are in-package, so
	// the seam needs no exported test-only method on the production type.
	s.oauthClock = clock.now
	policy := &fakeDPoPPolicy{}
	s.EnableOAuthResourceServer(policy, testPDSOrigin)
	return s, policy, clock
}

// testIssuerKey is one key of the nest's issuer set: a real P-256 signer and
// the RFC-7638 `kid` the nest names it by.
type testIssuerKey struct {
	priv *ecdsa.PrivateKey
	kid  string
}

func newTestIssuerKey(t *testing.T) *testIssuerKey {
	t.Helper()
	priv, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	kid, err := ecThumbprint(&priv.PublicKey)
	if err != nil {
		t.Fatal(err)
	}
	return &testIssuerKey{priv: priv, kid: kid}
}

// feed renders this key as a served-set entry, in the base64url coordinate
// spelling the nest's /oauth/jwks serves.
func (k *testIssuerKey) feed() NestIssuerJWK {
	pub := publicJWK(&k.priv.PublicKey)
	return NestIssuerJWK{Kid: k.kid, X: pub.X, Y: pub.Y}
}

// sign mints an ES256 access token under this key, the way the nest's
// authorization server does: `alg`/`typ`/`kid` header, fixed-width r‖s.
func (k *testIssuerKey) sign(t *testing.T, claims any) string {
	t.Helper()
	seg := func(v any) string {
		raw, err := json.Marshal(v)
		if err != nil {
			t.Fatal(err)
		}
		return base64.RawURLEncoding.EncodeToString(raw)
	}
	signing := seg(oauthJoseHeader{Alg: "ES256", Typ: oauthAccessTokenTyp, Kid: k.kid}) + "." + seg(claims)
	digest := sha256.Sum256([]byte(signing))
	r, sv, err := ecdsa.Sign(rand.Reader, k.priv, digest[:])
	if err != nil {
		t.Fatal(err)
	}
	sig := make([]byte, 2*p256CoordBytes)
	r.FillBytes(sig[:p256CoordBytes])
	sv.FillBytes(sig[p256CoordBytes:])
	return signing + "." + base64.RawURLEncoding.EncodeToString(sig)
}

// mintNestAccessToken signs a token bound to `signer`'s key, naming `iss`, for
// the approving test account — every claim the resource server reads.
func mintNestAccessToken(t *testing.T, s *Server, key *testIssuerKey, iss string, signer *testDPoPSigner) string {
	t.Helper()
	now := s.oauthClock()
	return key.sign(t, oauthAccessClaims{
		Iss:   iss,
		Sub:   testLoginDID,
		Aud:   oauthAudience{testServiceDID},
		Scope: "atproto",
		Iat:   now.Unix(),
		Exp:   now.Add(15 * time.Minute).Unix(),
		Jti:   "jti-0001",
		Cnf:   oauthCnf{Jkt: signer.jkt(t)},
		Sid:   base64.RawURLEncoding.EncodeToString([]byte("grant-family-id")),
		CliID: "https://app.example.com/client.json",
		Actor: hex.EncodeToString(testActorID),
	})
}

// honourNestIssuer feeds the resource server the nest's issuer and one key —
// the state of a claimed-domain deployment after its first key read.
func honourNestIssuer(t *testing.T, s *Server) *testIssuerKey {
	t.Helper()
	key := newTestIssuerKey(t)
	if err := s.SetNestIssuerKeys(nestIssuerURL, []NestIssuerJWK{key.feed()}); err != nil {
		t.Fatalf("SetNestIssuerKeys: %v", err)
	}
	return key
}

// rsRequest builds an authed XRPC request the way a real OAuth client does:
// `Authorization: DPoP <token>` plus a proof over this request carrying the
// token's own `ath` and a nonce minted right now.
func rsRequest(t *testing.T, s *Server, signer *testDPoPSigner, token, path string) *http.Request {
	t.Helper()
	return rsRequestWithNonce(t, s, signer, token, path, s.dpopNonces.Mint())
}

// rsRequestWithNonce is [rsRequest] with the nonce named by the caller.
//
// ⚠ **An inline mint is exactly what hid finding** — it hands every
// request a nonce that is fresh by construction, which is a thing no real
// client can do: a client echoes a nonce it was *given*, and can only have been
// given one by a response it already received. Any test about nonce freshness
// has to say which nonce, from which response, at what time.
func rsRequestWithNonce(t *testing.T, s *Server, signer *testDPoPSigner, token, path, nonce string) *http.Request {
	t.Helper()
	req := httptest.NewRequest(http.MethodGet, path, nil)
	req.Header.Set("Authorization", "DPoP "+token)
	req.Header.Set(dpopProofHeader, signer.rsProof(s, token, path, nonce))
	return req
}

// rsFrame serves one OAuth-plane route through the real XRPC frame, with a
// permissive authz hook: D8's scope matrix has its own tests, and the
// properties here live in the auth step that runs before it.
func rsFrame(s *Server) (http.Handler, string) {
	const path = "/xrpc/com.example.rs"
	allowAll := func(*http.Request, *xrpc.Route, *xrpc.Caller) *xrpc.Error { return nil }
	frame := xrpc.NewServer(nil, s.OAuthTokenVerifier(), allowAll, nil, nil)
	frame.Register(xrpc.Route{
		NSID: "com.example.rs", Method: http.MethodGet, Auth: xrpc.OAuthSession,
		Handle: func(w http.ResponseWriter, _ *http.Request, _ *xrpc.Caller) {
			xrpc.WriteJSON(w, map[string]any{"ok": true})
		}})
	return frame, path
}

// ── The headline ─────────────────────────────────────────────────────────────

// **A nest-minted access token authenticates an XRPC call, and only with its
// key.** The caller resolves to the approving account with the granted scopes,
// and D8 receives the OAuth plane, so the matrix judges it as an OAuth grant
// rather than an app password.
func TestANestMintedAccessTokenAuthenticatesAnXrpcCall(t *testing.T) {
	s, _, _ := rsServer(t)
	key := honourNestIssuer(t, s)
	token := mintNestAccessToken(t, s, key, nestIssuerURL, clientSigner)

	caller, err := s.OAuthTokenVerifier().VerifyAccess(
		rsRequest(t, s, clientSigner, token, "/xrpc/com.atproto.server.getSession"), token)
	if err != nil {
		t.Fatalf("a nest-minted access token must authenticate: %v", err)
	}
	if caller.DID != testLoginDID {
		t.Errorf("caller DID = %q, want the approving account's", caller.DID)
	}
	if caller.Plane != PlaneOAuth {
		t.Errorf("plane = %q, want %q — D8 judges the two planes differently", caller.Plane, PlaneOAuth)
	}
	if caller.Scope != "atproto" {
		t.Errorf("scope = %q, want the granted set (D8 splits it)", caller.Scope)
	}
	if string(caller.ActorID) != string(testActorID) {
		t.Error("the caller must resolve to the approving account's actor")
	}
}

// **⚠ The binding is only real if every one of these refuses**, and each is a
// different way of holding a token without holding its key — or of holding a
// token the issuer never meant for this resource server.
func TestABoundTokenIsUselessWithoutItsKeyAndItsProof(t *testing.T) {
	s, _, _ := rsServer(t)
	key := honourNestIssuer(t, s)
	token := mintNestAccessToken(t, s, key, nestIssuerURL, clientSigner)
	verifier := s.OAuthTokenVerifier()

	t.Run("no proof at all", func(t *testing.T) {
		req := httptest.NewRequest(http.MethodGet, "/xrpc/x", nil)
		req.Header.Set("Authorization", "DPoP "+token)
		if _, err := verifier.VerifyAccess(req, token); err == nil {
			t.Error("a DPoP-scheme presentation with no proof must be refused")
		}
	})

	t.Run("another key's proof", func(t *testing.T) {
		req := rsRequest(t, s, newTestDPoPSigner(), token, "/xrpc/x")
		if _, err := verifier.VerifyAccess(req, token); err == nil {
			t.Error("a proof from a key the token is not bound to must be refused — " +
				"otherwise cnf.jkt is decorative")
		}
	})

	t.Run("a token the issuer did not sign", func(t *testing.T) {
		forged := token[:len(token)-4] + "AAAA"
		req := rsRequest(t, s, clientSigner, forged, "/xrpc/x")
		if _, err := verifier.VerifyAccess(req, forged); err == nil {
			t.Error("a token whose signature does not verify must be refused")
		}
	})

	t.Run("a token minted for another service", func(t *testing.T) {
		now := s.oauthClock()
		other := key.sign(t, oauthAccessClaims{
			Iss: nestIssuerURL, Sub: testLoginDID, Aud: oauthAudience{"did:web:pds.elsewhere.example"},
			Scope: "atproto", Iat: now.Unix(), Exp: now.Add(15 * time.Minute).Unix(),
			Cnf: oauthCnf{Jkt: clientSigner.jkt(t)}, Sid: "c2lk", Actor: hex.EncodeToString(testActorID),
		})
		req := rsRequest(t, s, clientSigner, other, "/xrpc/x")
		if _, err := verifier.VerifyAccess(req, other); err == nil {
			t.Error("a token whose aud is another service must be refused here")
		}
	})
}

// **The audience is the set of the token's readers, and this PDS requires its
// own service DID to be a member** (`authorization-server.md` § The issuer →
// *The audience is the set of readers*). The nest spells one reader as a string
// and two as an array, so both spellings are read; the other members are the
// other readers' business and are ignored.
//
// The claims are raw JSON on purpose: what is under test is how this server
// reads the wire, so the fixture must not pass through the very type that
// does the reading.
func TestTheAudienceIsAMembershipCheckOverAStringOrAnArray(t *testing.T) {
	s, _, _ := rsServer(t)
	key := honourNestIssuer(t, s)
	verifier := s.OAuthTokenVerifier()

	cases := []struct {
		name string
		aud  any
		ok   bool
	}{
		{"a string naming this PDS", testServiceDID, true},
		{"an array naming this PDS and the nest", []string{testServiceDID, nestIssuerURL}, true},
		{"an array naming this PDS last", []string{nestIssuerURL, testServiceDID}, true},
		{"a one-member array naming this PDS", []string{testServiceDID}, true},
		{"a string naming the nest alone", nestIssuerURL, false},
		{"an array without this PDS", []string{nestIssuerURL, "did:web:pds.elsewhere.example"}, false},
		{"an empty array", []string{}, false},
		{"a different string", "did:web:pds.elsewhere.example", false},
		{"an audience that is neither", 7, false},
		{"an array with a non-string member", []any{testServiceDID, 7}, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			now := s.oauthClock()
			token := key.sign(t, map[string]any{
				"iss": nestIssuerURL, "sub": testLoginDID, "aud": tc.aud,
				"scope": "atproto", "iat": now.Unix(), "exp": now.Add(15 * time.Minute).Unix(),
				"jti": "jti-aud", "cnf": map[string]any{"jkt": clientSigner.jkt(t)},
				"sid":         base64.RawURLEncoding.EncodeToString([]byte("grant-family-id")),
				"fauna_actor": hex.EncodeToString(testActorID),
			})
			_, err := verifier.VerifyAccess(rsRequest(t, s, clientSigner, token, "/xrpc/x"), token)
			if tc.ok && err != nil {
				t.Errorf("a token naming this PDS among its readers must be honoured: %v", err)
			}
			if !tc.ok && err == nil {
				t.Error("a token that does not name this PDS must be refused here")
			}
		})
	}
}

// A resource server that was never wired refuses every token — and a wired
// one fed no issuer does too. The second is the domainless deployment: a nest
// with no claimed domain has no issuer, so there is nothing to honour.
func TestAnUnwiredOrIssuerlessResourceServerRefusesEveryToken(t *testing.T) {
	key := newTestIssuerKey(t)

	unwired := NewServer(nil, testMinter(t, nil), nil, nil, nil)
	if err := unwired.SetNestIssuerKeys(nestIssuerURL, []NestIssuerJWK{key.feed()}); err != nil {
		t.Fatal(err)
	}
	token := mintNestAccessToken(t, unwired, key, nestIssuerURL, clientSigner)
	req := httptest.NewRequest(http.MethodGet, "/xrpc/x", nil)
	if _, err := unwired.OAuthTokenVerifier().VerifyAccess(req, token); err == nil {
		t.Error("a server with no resource-server wiring verified an OAuth token")
	}

	s, _, _ := rsServer(t)
	token = mintNestAccessToken(t, s, key, nestIssuerURL, clientSigner)
	if _, err := s.OAuthTokenVerifier().VerifyAccess(rsRequest(t, s, clientSigner, token, "/xrpc/x"), token); err == nil {
		t.Error("a resource server fed no issuer verified an OAuth token")
	}
}

// **What the GO half owns on the resource-server path is the two facts it
// hands the policy module**, so that is what this asserts: the `htu` for THIS
// request (built from our own published origin, never the caller's Host) and
// the `ath` of THIS token. Whether a proof satisfying them is then accepted is
// the shared-Rust rule, tested there
// (`a_resource_server_proof_must_carry_the_presented_tokens_hash`) and pinned
// against this side by the cross-binary run.
//
// ⚠ Written this way deliberately. The obvious version — send a proof with a
// wrong `ath` and assert a refusal — would pass or fail on what the *fake*
// module does, testing the stub rather than the product: precisely the vacuous
// pin the finding-61 class describes.
func TestTheResourceServerGateAsksAboutThisRequestAndThisToken(t *testing.T) {
	s, policy, _ := rsServer(t)
	key := honourNestIssuer(t, s)
	token := mintNestAccessToken(t, s, key, nestIssuerURL, clientSigner)

	const path = "/xrpc/com.atproto.repo.createRecord"
	req := rsRequest(t, s, clientSigner, token, path)
	// The Host header is attacker-influenced; the expectations must not be.
	req.Host = "evil.example.com"
	if _, err := s.OAuthTokenVerifier().VerifyAccess(req, token); err != nil {
		t.Fatalf("verify: %v", err)
	}

	expect := policy.dpopExpect()
	if want := testPDSOrigin + path; expect.HTU != want {
		t.Errorf("htu = %q, want %q — built from our own published origin, never "+
			"the request's Host, or a caller reaching us under another name could "+
			"satisfy an equality check against a URL we never published", expect.HTU, want)
	}
	if expect.HTM != http.MethodGet {
		t.Errorf("htm = %q, want this request's method", expect.HTM)
	}
	sum := sha256.Sum256([]byte(token))
	if expect.ExpectedAth == nil {
		t.Fatal("a resource-server request must require `ath` — nil is the " +
			"authorization-server rule, where a proof carrying one is REFUSED")
	}
	if want := base64.RawURLEncoding.EncodeToString(sum[:]); *expect.ExpectedAth != want {
		t.Errorf("ath = %q, want the hash of the token this request presented", *expect.ExpectedAth)
	}
}

// **⚠ Every route's auth class is a claim about which PLANES may reach it.**
//
// `getSession` was registered `AppSession` while the app plane was the only
// one; the moment an OAuth token could exist, that made the base `atproto`
// scope unusable against the one method it is *defined* to cover. The tier_3
// four-process run caught it; nothing at unit level could, because each half
// was right about itself.
//
// This pins the shape rather than the one route: no route may be reachable by
// the app plane ONLY, unless something has decided that deliberately. Today
// nothing has, so the list is empty — and a future entry has to be added here
// on purpose, with a reason.
func TestNoRouteIsReachableByTheAppPlaneAlone(t *testing.T) {
	s := NewServer(nil, nil, nil, nil, nil)
	frame := xrpc.NewServer(nil, nil, nil, nil, nil)
	s.RegisterRoutes(frame)

	// Routes deliberately restricted to the app plane. Empty on purpose: an
	// entry here is a decision that an OAuth grant may never reach a method,
	// which needs its own ruling in § F4 detail's scope model.
	deliberate := map[string]bool{}

	for nsid, route := range frame.RegisteredRoutes() {
		if route.Auth != xrpc.AppSession || deliberate[nsid] {
			continue
		}
		t.Errorf("%s is registered AppSession, so an OAuth access token can never "+
			"reach it — no D8 scope check below it will ever run. Either register "+
			"it Session (both planes) or add it to `deliberate` with a ruling.", nsid)
	}
}

// **A client on the resource-server plane never gets stuck, because the plane
// that REQUIRES a nonce also ISSUES one.**
//
// Run through the real frame so it covers both halves of the repair — the
// `DPoP-Nonce` on every response and the retryable challenge that tells the
// client to use it. The arithmetic is the whole finding: a nonce lives
// `dpopProofMaxAge` (4 min), an access token 15 minutes, so without a nonce on
// every response every call from T+4min to T+15min would fail as the uniform
// `AuthenticationRequired`, indistinguishable from a forged token.
//
// ⚠ **The control leg is what makes the failing leg mean something**: at the
// same T+5min instant, the identical request carrying a nonce minted right then
// is accepted. So the refusal is the NONCE and nothing else — not the token's
// age, not `iat`, not `ath`, not `htu`.
func TestAnOAuthClientOnTheResourceServerPlaneNeverGetsStuckOnAStaleNonce(t *testing.T) {
	s, _, clock := rsServer(t)
	key := honourNestIssuer(t, s)
	token := mintNestAccessToken(t, s, key, nestIssuerURL, clientSigner)
	frame, path := rsFrame(s)

	call := func(nonce string) *httptest.ResponseRecorder {
		rec := httptest.NewRecorder()
		frame.ServeHTTP(rec, rsRequestWithNonce(t, s, clientSigner, token, path, nonce))
		return rec
	}

	// ── Leg 1: a call with a nonce this server issued works, and hands back
	// the next one.
	first := call(s.dpopNonces.Mint())
	if first.Code != http.StatusOK {
		t.Fatalf("a call with a freshly issued nonce = %d, want 200", first.Code)
	}
	harvested := first.Header().Get("DPoP-Nonce")
	if harvested == "" {
		t.Fatal("the success carried no DPoP-Nonce — a client has no source of " +
			"nonces on this plane (contract item 1)")
	}

	// ── Five minutes pass. The token is still live (15 min); the harvested
	// nonce is not (4 min).
	clock.advance(5 * time.Minute)

	// ── Control: a nonce minted at THIS instant is accepted.
	if got := call(s.dpopNonces.Mint()).Code; got != http.StatusOK {
		t.Fatalf("the control leg = %d, want 200 — with a fresh nonce this exact "+
			"request must still work at T+5min, or the test is measuring the "+
			"token's age rather than the nonce's", got)
	}

	// ── Leg 2: the same request with the nonce the client actually holds. It
	// is refused — correctly — but the refusal must be RECOVERABLE.
	stale := call(harvested)
	if stale.Code != http.StatusUnauthorized {
		t.Fatalf("a 5-minute-old nonce = %d, want 401 — accepting it would delete "+
			"the replay defence rather than fix the availability bug", stale.Code)
	}
	if got, want := stale.Header().Get("WWW-Authenticate"), `DPoP error="use_dpop_nonce"`; got != want {
		t.Errorf("challenge = %q, want %q — without it the client reads a "+
			"permanent auth failure, byte-identical to a forged token "+
			"(contract item 2)", got, want)
	}
	fresh := stale.Header().Get("DPoP-Nonce")
	if fresh == "" {
		t.Fatal("the refusal carried no DPoP-Nonce — the one response that must " +
			"carry one, since it is the response telling the client to retry")
	}
	if fresh == harvested {
		t.Error("the refusal echoed the stale nonce back; a retry would loop forever")
	}

	// ── Leg 3: the retry the challenge invites succeeds.
	if got := call(fresh).Code; got != http.StatusOK {
		t.Fatalf("the retry with the nonce from the refusal = %d, want 200 — a "+
			"client that does exactly what the challenge says must succeed", got)
	}
}

// The plane keeps issuing even when it refuses for an unrelated reason: a
// forged token gets no challenge (the uniform rule holds) but still gets a
// nonce, so the header's presence never tells the two refusals apart.
func TestAForgedTokenIsStillIssuedANonceButNeverAChallenge(t *testing.T) {
	s, _, _ := rsServer(t)
	key := honourNestIssuer(t, s)
	token := mintNestAccessToken(t, s, key, nestIssuerURL, clientSigner)
	forged := token[:len(token)-4] + "AAAA"
	frame, path := rsFrame(s)

	rec := httptest.NewRecorder()
	frame.ServeHTTP(rec, rsRequestWithNonce(t, s, clientSigner, forged, path, s.dpopNonces.Mint()))
	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("a forged token = %d, want 401", rec.Code)
	}
	if rec.Header().Get("DPoP-Nonce") == "" {
		t.Error("no nonce on a forged-token refusal — issuing only on SOME " +
			"refusals makes the header itself a distinguisher")
	}
	if got := rec.Header().Get("WWW-Authenticate"); got != "" {
		t.Errorf("a forged token was answered %q — only the nonce refusal is "+
			"distinguishable, and a token oracle is exactly what the uniform "+
			"rule denies", got)
	}
}
