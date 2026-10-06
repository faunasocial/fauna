package atprotopds

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

// ── A real DPoP client ───────────────────────────────────────────────────────

// testDPoPSigner is a client that holds a real P-256 key and mints real
// proofs. Nothing here is a stub: the signatures these tests verify are
// genuine ECDSA over genuine JOSE bytes, so `verifyDPoPSignature` and
// `ecThumbprint` are exercised against material a real client would send.
//
// What IS stubbed is the shared-Rust policy module — see
// [decodeProofLikeTheModuleDoes]. The two sides are pinned against each other
// by the cross-binary test in cmd/fauna-atproto-bridge, which runs a proof
// this same signer minted through the REAL Rust validator over the FFI.
type testDPoPSigner struct {
	key *ecdsa.PrivateKey
}

func newTestDPoPSigner() *testDPoPSigner {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		panic(fmt.Sprintf("generate test DPoP key: %v", err))
	}
	return &testDPoPSigner{key: key}
}

// clientSigner is the key the ordinary resource-server helpers prove possession
// with. One per test process is safe: every test builds its own Server, so no
// nonce secret or replay set is shared between them.
var clientSigner = newTestDPoPSigner()

func (d *testDPoPSigner) jwk() map[string]string {
	var x, y [32]byte
	d.key.X.FillBytes(x[:])
	d.key.Y.FillBytes(y[:])
	return map[string]string{
		"kty": "EC",
		"crv": "P-256",
		"x":   base64.RawURLEncoding.EncodeToString(x[:]),
		"y":   base64.RawURLEncoding.EncodeToString(y[:]),
	}
}

// jkt is the thumbprint the server should derive from a proof this signer made.
func (d *testDPoPSigner) jkt(t *testing.T) string {
	t.Helper()
	thumb, err := ecThumbprint(&d.key.PublicKey)
	if err != nil {
		t.Fatalf("ecThumbprint: %v", err)
	}
	return thumb
}

// sign assembles and signs a compact proof from the given header and claims.
func (d *testDPoPSigner) sign(header, claims map[string]any) string {
	seg := func(v any) string {
		raw, err := json.Marshal(v)
		if err != nil {
			panic(err)
		}
		return base64.RawURLEncoding.EncodeToString(raw)
	}
	signingInput := seg(header) + "." + seg(claims)
	digest := sha256.Sum256([]byte(signingInput))
	r, s, err := ecdsa.Sign(rand.Reader, d.key, digest[:])
	if err != nil {
		panic(fmt.Sprintf("sign test DPoP proof: %v", err))
	}
	var sig [64]byte
	r.FillBytes(sig[:32])
	s.FillBytes(sig[32:])
	return signingInput + "." + base64.RawURLEncoding.EncodeToString(sig[:])
}

func (d *testDPoPSigner) header() map[string]any {
	return map[string]any{"typ": "dpop+jwt", "alg": "ES256", "jwk": d.jwk()}
}

// rsProof mints a proof for a GET to `path` on this PDS, carrying the given
// nonce and the `ath` of `token`, with a fresh `jti` so repeated calls are not
// replays of each other.
func (d *testDPoPSigner) rsProof(s *Server, token, path, nonce string) string {
	return d.sign(d.header(), d.rsClaims(s, token, path, nonce))
}

// rsClaims is the claim set rsProof signs, exposed so a test can edit one claim
// (the `jti`, typically) before signing.
func (d *testDPoPSigner) rsClaims(s *Server, token, path, nonce string) map[string]any {
	var jti [12]byte
	if _, err := rand.Read(jti[:]); err != nil {
		panic(err)
	}
	sum := sha256.Sum256([]byte(token))
	return map[string]any{
		"jti":   base64.RawURLEncoding.EncodeToString(jti[:]),
		"htm":   http.MethodGet,
		"htu":   s.pdsOrigin + path,
		"iat":   s.oauthClock().Unix(),
		"nonce": nonce,
		"ath":   base64.RawURLEncoding.EncodeToString(sum[:]),
	}
}

// ── The stand-in for the shared-Rust policy module ───────────────────────────

// decodeProofLikeTheModuleDoes stands in for
// `fauna_bridge_atproto::dpop::validate_dpop_proof`. It performs the same
// decomposition and the one refusal the Go half branches on (`use_dpop_nonce`
// for an absent nonce) — and deliberately nothing else.
//
// The claim checks (`typ`, `alg`, `htm`, `htu`, `iat`, `ath`, the JWK shape)
// are the Rust module's and are tested there. What these tests own is the Go
// half: nonce recognition, replay, signature verification, thumbprinting, and
// the ORDER the four happen in.
func decodeProofLikeTheModuleDoes(compact string) DPoPVerdict {
	bad := func(description string) DPoPVerdict {
		return DPoPVerdict{Deny: &OAuthDeny{Error: oauthErrInvalidDPoP, Description: description}}
	}
	parts := strings.Split(compact, ".")
	if len(parts) != 3 {
		return bad("not a three-part compact JWS")
	}
	claimsRaw, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return bad("claims are not unpadded base64url")
	}
	headerRaw, err := base64.RawURLEncoding.DecodeString(parts[0])
	if err != nil {
		return bad("header is not unpadded base64url")
	}
	var header struct {
		JWK struct{ X, Y string } `json:"jwk"`
	}
	if err := json.Unmarshal(headerRaw, &header); err != nil {
		return bad("header is not JSON")
	}
	var claims struct {
		JTI   string `json:"jti"`
		Nonce string `json:"nonce"`
		IAT   int64  `json:"iat"`
	}
	if err := json.Unmarshal(claimsRaw, &claims); err != nil {
		return bad("claims are not JSON")
	}
	if claims.Nonce == "" {
		return DPoPVerdict{Deny: &OAuthDeny{
			Error:       oauthErrUseDPoPNonce,
			Description: "no server-issued nonce",
		}}
	}
	x, err := base64.RawURLEncoding.DecodeString(header.JWK.X)
	if err != nil {
		return bad("jwk.x is not unpadded base64url")
	}
	y, err := base64.RawURLEncoding.DecodeString(header.JWK.Y)
	if err != nil {
		return bad("jwk.y is not unpadded base64url")
	}
	sig, err := base64.RawURLEncoding.DecodeString(parts[2])
	if err != nil {
		return bad("signature is not unpadded base64url")
	}
	return DPoPVerdict{Proof: &DPoPProof{
		SigningInput: parts[0] + "." + parts[1],
		Signature:    sig,
		PublicKeyX:   x,
		PublicKeyY:   y,
		JTI:          claims.JTI,
		Nonce:        claims.Nonce,
		IssuedAt:     claims.IAT,
	}}
}

// ── The nonce ────────────────────────────────────────────────────────────────

// A nonce is recognised while its window is live and not afterwards — and the
// bound is the window index inside the MAC, so no amount of traffic can extend
// it.
func TestADpopNonceIsRecognisedWithinItsWindowAndNotAfter(t *testing.T) {
	clock := newTestClock()
	minter := newDPoPNonceMinter(clock.now)

	nonce := minter.Mint()
	if !minter.Accepts(nonce) {
		t.Fatal("a nonce must be accepted in the window it was minted in")
	}
	// Still inside the accepted span…
	clock.advance(dpopNonceWindow)
	if !minter.Accepts(nonce) {
		t.Fatal("a nonce must survive one window rollover — otherwise a client " +
			"that did nothing wrong is refused for crossing a boundary in flight")
	}
	// …and past it.
	clock.advance(dpopProofMaxAge)
	if minter.Accepts(nonce) {
		t.Fatal("a nonce outlived its window")
	}
}

// The five-minute ceiling in the spec is a maximum, and the constants must sit
// under it by construction rather than by a comment claiming they do.
func TestTheNonceLifetimeStaysUnderTheSpecCeiling(t *testing.T) {
	const specCeiling = 5 * time.Minute
	if dpopProofMaxAge > specCeiling {
		t.Fatalf("a nonce may live %s, past the spec's %s ceiling", dpopProofMaxAge, specCeiling)
	}
	if dpopProofMaxAge != dpopNonceWindow*dpopNonceAcceptedWindows {
		t.Fatal("the proof `iat` window and the nonce lifetime must be one constant — " +
			"two freshness rules on one path is two things to keep in step")
	}
}

// A nonce nobody issued is refused, which is the whole point of the MAC.
func TestANonceThisServerDidNotIssueIsRefused(t *testing.T) {
	clock := newTestClock()
	mine := newDPoPNonceMinter(clock.now)
	theirs := newDPoPNonceMinter(clock.now)
	if mine.Accepts(theirs.Mint()) {
		t.Fatal("a nonce minted under another secret was accepted")
	}
	for _, forged := range []string{"", "not-a-nonce", strings.Repeat("A", 24)} {
		if mine.Accepts(forged) {
			t.Fatalf("forged nonce %q was accepted", forged)
		}
	}
}

// **The reason the previous secret is kept.** A nonce minted moments before the
// rotation period elapses must still verify after it — otherwise every rotation
// refuses a burst of in-flight clients for no reason they could act on.
func TestANonceMintedJustBeforeRotationSurvivesIt(t *testing.T) {
	clock := newTestClock()
	minter := newDPoPNonceMinter(clock.now)

	clock.advance(dpopNonceSecretRotation - time.Second)
	nonce := minter.Mint() // still under the original secret
	clock.advance(2 * time.Second)

	if !minter.Accepts(nonce) {
		t.Fatal("a nonce was refused across a secret rotation — the one-generation " +
			"overlap exists precisely for the client that was mid-flight")
	}
	// And the rotation really happened: a fresh nonce is minted under a secret
	// the pre-rotation one was not.
	if minter.Mint() == nonce {
		t.Fatal("the secret did not rotate")
	}
}

// ── The replay set ───────────────────────────────────────────────────────────

func TestADpopProofIdentifierIsAcceptedOnceAndThenRefused(t *testing.T) {
	clock := newTestClock()
	set := newReplaySet("dpop-proof-rs", dpopReplayCapacity, clock.now, nil)
	expiry := func() time.Time { return clock.now().Add(dpopProofMaxAge) }
	if set.Record("scope-a", "jti-1", expiry()) {
		t.Fatal("a first use must not read as a replay")
	}
	if !set.Record("scope-a", "jti-1", expiry()) {
		t.Fatal("a second use of the same jti must read as a replay")
	}
	// Past the window a proof could be accepted in, the entry is no longer
	// needed — the nonce would refuse the proof long before the jti had to.
	clock.advance(dpopProofMaxAge + time.Second)
	if set.Record("scope-a", "jti-1", expiry()) {
		t.Fatal("an expired entry must not keep answering")
	}
}

// The entry's life is the CALLER's, not a store-wide constant: the set forgets
// an identifier exactly when the caller said replaying it stops mattering.
func TestAReplayEntryExpiresOnTheLifetimeItsCallerSupplied(t *testing.T) {
	clock := newTestClock()
	set := newReplaySet("test", 64, clock.now, nil)
	set.Record("scope-a", "short", clock.now().Add(time.Minute))
	set.Record("scope-a", "long", clock.now().Add(time.Hour))
	clock.advance(2 * time.Minute)
	if set.Record("scope-a", "short", clock.now().Add(time.Minute)) {
		t.Fatal("the short-lived entry must have expired on its own lifetime")
	}
	if !set.Record("scope-a", "long", clock.now().Add(time.Hour)) {
		t.Fatal("the long-lived entry must still answer — one store, two policies")
	}
}

// ⚠ at the unit level: one caller's identifier must not answer for
// another's. Every identifier this set holds is caller-chosen and RFC 9449 does
// not make one globally unique, so a bare key lets any caller spend an
// identifier an honest caller is about to present.
func TestOneScopesIdentifierDoesNotAnswerForAnothers(t *testing.T) {
	clock := newTestClock()
	set := newReplaySet("test", 64, clock.now, nil)
	expiry := func() time.Time { return clock.now().Add(time.Minute) }

	if set.Record("scope-a", "shared-jti", expiry()) {
		t.Fatal("a first use must not read as a replay")
	}
	if set.Record("scope-b", "shared-jti", expiry()) {
		t.Fatal("BURNED: another scope's identifier locked this one out")
	}
	// …and within one scope it still refuses, so scoping did not disable it.
	if !set.Record("scope-a", "shared-jti", expiry()) {
		t.Fatal("a genuine replay within one scope must still be refused")
	}
	if !set.Record("scope-b", "shared-jti", expiry()) {
		t.Fatal("a genuine replay within the other scope must still be refused")
	}
}

// The key encoding is injective **over the scopes this set is actually given**,
// which is what makes the scoping above a boundary rather than a concatenation
// an attacker can spoof. The argument is one-sided on purpose: a scope is
// NUL-free by construction (a base64url thumbprint), so the FIRST NUL always
// delimits and the scope is recoverable from the key — while the identifier
// half, the half a caller chooses, is allowed to contain anything at all.
func TestAnIdentifierCannotSpellAnotherScopesKey(t *testing.T) {
	victim := clientSigner.jkt(t)
	attacker := newTestDPoPSigner().jkt(t)
	for _, id := range []string{"plain", "with\x00nul", "\x00", "a\x00b\x00c", ""} {
		key := replayKey(victim, id)
		scope, _, found := strings.Cut(key, "\x00")
		if !found || scope != victim {
			t.Fatalf("scope not recoverable from the key for identifier %q", id)
		}
		if key == replayKey(attacker, id) {
			t.Fatalf("two scopes produced one key for identifier %q", id)
		}
	}
}

func TestTheReplaySetIsBoundedAndPrefersDroppingExpiredEntries(t *testing.T) {
	clock := newTestClock()
	set := newReplaySet("dpop-proof-rs", dpopReplayCapacity, clock.now, nil)
	for i := 0; i < dpopReplayCapacity+64; i++ {
		set.Record("scope-a", fmt.Sprintf("jti-%d", i), clock.now().Add(dpopProofMaxAge))
	}
	if n := len(set.seen); n > dpopReplayCapacity {
		t.Fatalf("replay set grew to %d entries, past its %d ceiling", n, dpopReplayCapacity)
	}
}

// Item 3 of the contract: evicting a LIVE entry is the only moment the
// replay defence is degraded, and the log line is its only witness — so it must
// fire, and it must name its set, so an admin reading the log knows which
// defence degraded.
func TestEvictingALiveEntryWarnsAndNamesItsSet(t *testing.T) {
	clock := newTestClock()
	var logged strings.Builder
	logger := slog.New(slog.NewTextHandler(&logged, &slog.HandlerOptions{Level: slog.LevelWarn}))
	set := newReplaySet("dpop-proof-rs", 2, clock.now, logger)

	// Three live entries into a set of two: nothing has expired, so the third
	// insert must evict a live one.
	for i := 0; i < 3; i++ {
		set.Record("scope-a", fmt.Sprintf("jti-%d", i), clock.now().Add(dpopProofMaxAge))
	}
	line := logged.String()
	if line == "" {
		t.Fatal("evicting a live entry logged nothing — the degraded window is invisible")
	}
	if !strings.Contains(line, "dpop-proof-rs") {
		t.Errorf("the eviction warning does not name its set: %q", line)
	}

	// An eviction that only dropped EXPIRED entries is not a degraded window and
	// must stay quiet, or the signal drowns in its own noise.
	logged.Reset()
	clock.advance(dpopProofMaxAge + time.Second)
	set.Record("scope-a", "jti-later", clock.now().Add(dpopProofMaxAge))
	if logged.String() != "" {
		t.Errorf("sweeping expired entries is routine and must not warn: %q", logged.String())
	}
}

// ── Signature verification ───────────────────────────────────────────────────

// The signature is checked over the module's signing input, so a tampered
// header or claim set cannot ride a valid signature.
func TestASignatureIsVerifiedOverTheBytesTheModuleHandedBack(t *testing.T) {
	signer := newTestDPoPSigner()
	compact := signer.sign(signer.header(), map[string]any{
		"jti": "abc", "htm": "GET", "htu": "https://pds.example.com/xrpc/com.atproto.server.getSession",
		"iat": int64(1_800_000_000), "nonce": "n",
	})
	verdict := decodeProofLikeTheModuleDoes(compact)
	if verdict.Proof == nil {
		t.Fatalf("fixture proof must decode: %+v", verdict.Deny)
	}
	if _, ok := verifyDPoPSignature(verdict.Proof); !ok {
		t.Fatal("a genuine proof must verify")
	}

	tampered := *verdict.Proof
	tampered.SigningInput += "x"
	if _, ok := verifyDPoPSignature(&tampered); ok {
		t.Fatal("a signature verified over bytes it does not cover")
	}

	wrongKey := *verdict.Proof
	other := newTestDPoPSigner()
	var x, y [32]byte
	other.key.X.FillBytes(x[:])
	other.key.Y.FillBytes(y[:])
	wrongKey.PublicKeyX, wrongKey.PublicKeyY = x[:], y[:]
	if _, ok := verifyDPoPSignature(&wrongKey); ok {
		t.Fatal("a proof verified under a key that did not sign it")
	}
}

// A point that is not on P-256 must never reach the thumbprint: `jkt` is
// computed from the coordinates, and a nonsense point must not become a binding
// a later access token carries.
func TestAKeyThatIsNotOnTheCurveIsRefused(t *testing.T) {
	signer := newTestDPoPSigner()
	compact := signer.sign(signer.header(), map[string]any{"jti": "a", "nonce": "n"})
	verdict := decodeProofLikeTheModuleDoes(compact)
	proof := *verdict.Proof
	proof.PublicKeyY = make([]byte, 32) // (x, 0) is not on the curve
	if _, ok := verifyDPoPSignature(&proof); ok {
		t.Fatal("an off-curve point was accepted")
	}
}

// ── The gate ─────────────────────────────────────────────────────────────────

// gateRequest builds a request carrying the given proofs (none, one or more).
func gateRequest(path string, proofs ...string) *http.Request {
	req := httptest.NewRequest(http.MethodGet, path, nil)
	for _, p := range proofs {
		req.Header.Add(dpopProofHeader, p)
	}
	return req
}

const gatePath = "/xrpc/com.atproto.server.getSession"

// No proof, and two proofs, are both refused before the policy module is
// consulted: two `DPoP` headers is a request that means two different things
// to two different readers.
func TestTheGateRefusesAMissingOrDoubledProof(t *testing.T) {
	s, policy, _ := rsServer(t)
	if _, deny := s.dpopGate(gateRequest(gatePath), testPDSOrigin+gatePath, "ath"); deny == nil ||
		deny.Error != oauthErrInvalidDPoP {
		t.Fatalf("no proof: deny = %+v, want %q", deny, oauthErrInvalidDPoP)
	}
	proof := clientSigner.rsProof(s, "tok", gatePath, s.dpopNonces.Mint())
	if _, deny := s.dpopGate(gateRequest(gatePath, proof, proof), testPDSOrigin+gatePath, "ath"); deny == nil ||
		deny.Error != oauthErrInvalidDPoP {
		t.Fatalf("doubled proof: deny = %+v, want %q", deny, oauthErrInvalidDPoP)
	}
	if policy.calls() != 0 {
		t.Fatalf("the policy module was consulted %d time(s) for a request with no single proof", policy.calls())
	}
}

// **The first-contact round trip.** A proof with no nonce, or with one this
// server never issued, is refused as the retryable `use_dpop_nonce` — the
// client's fix is to use the one the response hands it.
func TestTheGateAnswersUseDpopNonceForAMissingOrForeignNonce(t *testing.T) {
	s, _, _ := rsServer(t)
	for name, nonce := range map[string]string{"absent": "", "foreign": "a-nonce-we-never-minted"} {
		t.Run(name, func(t *testing.T) {
			proof := clientSigner.rsProof(s, "tok", gatePath, nonce)
			_, deny := s.dpopGate(gateRequest(gatePath, proof), testPDSOrigin+gatePath, "ath")
			if deny == nil || deny.Error != oauthErrUseDPoPNonce {
				t.Fatalf("deny = %+v, want %q", deny, oauthErrUseDPoPNonce)
			}
		})
	}
	proof := clientSigner.rsProof(s, "tok", gatePath, s.dpopNonces.Mint())
	if _, deny := s.dpopGate(gateRequest(gatePath, proof), testPDSOrigin+gatePath, "ath"); deny != nil {
		t.Fatalf("a proof carrying an issued nonce was refused: %+v", deny)
	}
}

// A proof cannot be spent twice.
func TestAReplayedProofIsRefused(t *testing.T) {
	s, _, _ := rsServer(t)
	proof := clientSigner.rsProof(s, "tok", gatePath, s.dpopNonces.Mint())
	jkt, deny := s.dpopGate(gateRequest(gatePath, proof), testPDSOrigin+gatePath, "ath")
	if deny != nil {
		t.Fatalf("first use must pass: %+v", deny)
	}
	if want := clientSigner.jkt(t); jkt != want {
		t.Fatalf("gate jkt = %q, want the proving key's thumbprint %q", jkt, want)
	}
	if _, deny := s.dpopGate(gateRequest(gatePath, proof), testPDSOrigin+gatePath, "ath"); deny == nil ||
		deny.Error != oauthErrInvalidDPoP {
		t.Fatalf("a replayed proof: deny = %+v, want %q", deny, oauthErrInvalidDPoP)
	}
}

// **The ordering that makes the replay check safe.** A `jti` is recorded only
// after the signature verifies. Were it recorded first, anyone could send
// unsigned proofs carrying an honest client's *future* identifiers and lock
// that client out — the defence would become the attack.
func TestAFailedProofDoesNotBurnItsIdentifier(t *testing.T) {
	s, _, _ := rsServer(t)

	genuine := clientSigner.rsProof(s, "tok", gatePath, s.dpopNonces.Mint())
	parts := strings.Split(genuine, ".")
	// Same header, same claims — same `jti` — but a signature that does not
	// verify. This is exactly what an attacker replaying a captured claim set
	// under their own (absent) key would send.
	forged := parts[0] + "." + parts[1] + "." +
		base64.RawURLEncoding.EncodeToString(make([]byte, 64))

	if _, deny := s.dpopGate(gateRequest(gatePath, forged), testPDSOrigin+gatePath, "ath"); deny == nil {
		t.Fatal("a forged signature must be refused")
	}
	if _, deny := s.dpopGate(gateRequest(gatePath, genuine), testPDSOrigin+gatePath, "ath"); deny != nil {
		t.Fatalf("the genuine proof was locked out by the forgery: %+v", deny)
	}
}

// ⚠⚠ **The other half of that threat model**. `TestAFailedProofDoesNotBurnItsIdentifier` above closes
// the case where the attacker's *own* proof is invalid. A **validly signed**
// proof from a **different key** burned an honest caller's identifier just as
// effectively while the replay set was keyed on the bare caller-chosen `jti`.
// The set is now keyed by the proving key's thumbprint.
func TestAnotherKeysValidProofDoesNotBurnThisCallersIdentifier(t *testing.T) {
	s, _, _ := rsServer(t)
	attacker := newTestDPoPSigner()

	// The same identifier, presented by two different keys. An honest client
	// picking `jti`s from a counter — which no spec forbids — is exactly this.
	const sharedJTI = "the-victims-next-identifier"
	proof := func(d *testDPoPSigner) string {
		c := d.rsClaims(s, "tok", gatePath, s.dpopNonces.Mint())
		c["jti"] = sharedJTI
		return d.sign(d.header(), c)
	}

	if _, deny := s.dpopGate(gateRequest(gatePath, proof(attacker)), testPDSOrigin+gatePath, "ath"); deny != nil {
		t.Fatalf("the attacker's own proof must be valid for this test to mean anything: %+v", deny)
	}
	honest := proof(clientSigner)
	if _, deny := s.dpopGate(gateRequest(gatePath, honest), testPDSOrigin+gatePath, "ath"); deny != nil {
		t.Fatalf("BURNED: another key's proof jti locked this caller out: %+v", deny)
	}
	// …and the caller's own replay is still refused, so scoping did not
	// disable the defence it narrowed.
	if _, deny := s.dpopGate(gateRequest(gatePath, honest), testPDSOrigin+gatePath, "ath"); deny == nil {
		t.Fatal("a genuine self-replay must still be refused")
	}
}
