package main

import (
	"bytes"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	faunaAtproto "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_bridge_atproto"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotopds"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// F4 slice 4's cross-binary agreement pin.
//
// The in-package tests on either side prove each half against its OWN fixtures:
// `libs/fauna-bridge-atproto/src/dpop.rs` builds proofs with serde_json and
// checks its own decode, and `internal/atprotopds/dpop_test.go` verifies
// signatures over material a test-local stand-in decoded. Neither can see a
// disagreement between the real encoder and the real decoder — which is exactly
// the class that let the blob walk return `[]` for every real record for weeks
// with every unit test green.
//
// So these tests mint a proof with Go's real `crypto/ecdsa` and JOSE encoding,
// hand it to the REAL Rust validator over the FFI, and assert the two sides
// agree — first byte-for-byte at the seam, then end-to-end through the
// resource server's production verify path.

// crossBinarySigner is an ATProto client that holds a real P-256 key.
//
// Deliberately NOT the `internal/atprotopds` test signer: this file's whole job
// is to be an independent producer, so sharing a helper with the side under
// test would reintroduce the "both fixtures came from the same helper" problem
// it exists to rule out.
type crossBinarySigner struct{ key *ecdsa.PrivateKey }

func newCrossBinarySigner(t *testing.T) *crossBinarySigner {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatalf("generate P-256 key: %v", err)
	}
	return &crossBinarySigner{key: key}
}

func (c *crossBinarySigner) coords() (x, y [32]byte) {
	c.key.X.FillBytes(x[:])
	c.key.Y.FillBytes(y[:])
	return x, y
}

// jkt computes the RFC-7638 thumbprint independently of the production
// implementation — canonical JSON of exactly {crv, kty, x, y}, SHA-256,
// base64url. Recomputing it here rather than calling the bridge's own helper is
// the point: the assertion is against the standard, not against ourselves.
func (c *crossBinarySigner) jkt(t *testing.T) string {
	t.Helper()
	x, y := c.coords()
	canonical := fmt.Sprintf(
		`{"crv":"P-256","kty":"EC","x":%q,"y":%q}`,
		base64.RawURLEncoding.EncodeToString(x[:]),
		base64.RawURLEncoding.EncodeToString(y[:]),
	)
	sum := sha256.Sum256([]byte(canonical))
	return base64.RawURLEncoding.EncodeToString(sum[:])
}

func (c *crossBinarySigner) header() map[string]any {
	x, y := c.coords()
	return map[string]any{
		"typ": "dpop+jwt",
		"alg": "ES256",
		"jwk": map[string]string{
			"kty": "EC",
			"crv": "P-256",
			"x":   base64.RawURLEncoding.EncodeToString(x[:]),
			"y":   base64.RawURLEncoding.EncodeToString(y[:]),
		},
	}
}

// sign assembles a compact JWS the way any JOSE library would.
func (c *crossBinarySigner) sign(t *testing.T, header, claims map[string]any) string {
	t.Helper()
	seg := func(v any) string {
		raw, err := json.Marshal(v)
		if err != nil {
			t.Fatalf("marshal segment: %v", err)
		}
		return base64.RawURLEncoding.EncodeToString(raw)
	}
	signingInput := seg(header) + "." + seg(claims)
	digest := sha256.Sum256([]byte(signingInput))
	r, s, err := ecdsa.Sign(rand.Reader, c.key, digest[:])
	if err != nil {
		t.Fatalf("sign proof: %v", err)
	}
	var sig [64]byte
	r.FillBytes(sig[:32])
	s.FillBytes(sig[32:])
	return signingInput + "." + base64.RawURLEncoding.EncodeToString(sig[:])
}

const (
	crossBinaryApex = "example.com"
	crossBinaryPath = "/xrpc/com.atproto.server.getSession"
	// crossBinaryAth stands in for `base64url(sha256(access token))` at the
	// seam-level pins, where no token is in play.
	crossBinaryAth = "fUHyO2r2Z3DZ53EsNrWBb0xWXoaNy59IiKCAqksmQEo"
)

// crossBinaryPDSOrigin is the origin a resource-server proof's `htu` is built
// from — through the same shared-Rust builders main.go wires.
func crossBinaryPDSOrigin() string {
	return faunaAtproto.OauthIssuer(faunaAtproto.AtprotoPdsHost(crossBinaryApex))
}

// crossBinaryExpectations are the request facts the resource server would
// supply, with a fixed clock so the seam-level assertions below do not depend
// on wall time.
func crossBinaryExpectations() atprotopds.DPoPExpectations {
	ath := crossBinaryAth
	return atprotopds.DPoPExpectations{
		HTM:         http.MethodGet,
		HTU:         crossBinaryPDSOrigin() + crossBinaryPath,
		NowUnix:     1_800_000_000,
		MaxAgeSecs:  240,
		MaxSkewSecs: 30,
		ExpectedAth: &ath,
	}
}

// **The seam-level pin: what Go encoded is what Rust decoded.**
//
// A proof minted by real ECDSA and real JOSE encoding, run through the real
// shared-Rust validator, must come back decomposed into exactly the pieces Go
// put in — the same signing input, the same key coordinates, the same claims.
// Each side's own tests can pass while these disagree; only this can see it.
func TestTheRustDpopValidatorDecomposesWhatTheGoEncoderReallyWrote(t *testing.T) {
	signer := newCrossBinarySigner(t)
	expect := crossBinaryExpectations()
	claims := map[string]any{
		"jti":   "cross-binary-proof-1",
		"htm":   expect.HTM,
		"htu":   expect.HTU,
		"iat":   expect.NowUnix,
		"nonce": "a-nonce-the-server-issued",
		"ath":   crossBinaryAth,
	}
	compact := signer.sign(t, signer.header(), claims)

	verdict := ffiDPoPPolicy{}.ValidateDPoPProof(compact, expect)
	if verdict.Proof == nil {
		t.Fatalf("the real Rust validator refused a genuine proof: %+v", verdict.Deny)
	}

	wantSigningInput := compact[:strings.LastIndex(compact, ".")]
	if verdict.Proof.SigningInput != wantSigningInput {
		t.Errorf("signing input disagreed across the FFI:\n go:   %q\n rust: %q",
			wantSigningInput, verdict.Proof.SigningInput)
	}
	x, y := signer.coords()
	if !bytes.Equal(verdict.Proof.PublicKeyX, x[:]) || !bytes.Equal(verdict.Proof.PublicKeyY, y[:]) {
		t.Error("the key coordinates Rust decoded are not the ones Go encoded")
	}
	if verdict.Proof.JTI != "cross-binary-proof-1" ||
		verdict.Proof.Nonce != "a-nonce-the-server-issued" ||
		verdict.Proof.IssuedAt != expect.NowUnix {
		t.Errorf("claims disagreed across the FFI: %+v", verdict.Proof)
	}
}

// The real Rust policy is genuinely wired — not a Go-side lookalike. Each of
// these is a refusal only the module can produce, driven through the production
// adapter.
func TestTheRustDpopPolicyIsTheOneTheBridgeConsults(t *testing.T) {
	signer := newCrossBinarySigner(t)
	expect := crossBinaryExpectations()
	good := func() map[string]any {
		return map[string]any{
			"jti": "x", "htm": expect.HTM, "htu": expect.HTU,
			"iat": expect.NowUnix, "nonce": "n", "ath": crossBinaryAth,
		}
	}

	for _, tc := range []struct {
		name      string
		header    func(map[string]any) map[string]any
		claims    func(map[string]any) map[string]any
		wantError string
	}{
		{
			name:      "an unsigned proof",
			header:    func(h map[string]any) map[string]any { h["alg"] = "none"; return h },
			wantError: "invalid_dpop_proof",
		},
		{
			name:      "a proof for another endpoint",
			claims:    func(c map[string]any) map[string]any { c["htu"] = crossBinaryPDSOrigin() + "/xrpc/other"; return c },
			wantError: "invalid_dpop_proof",
		},
		{
			name:      "a proof with no nonce",
			claims:    func(c map[string]any) map[string]any { delete(c, "nonce"); return c },
			wantError: "use_dpop_nonce",
		},
		{
			name:      "a proof carrying private key material",
			header:    func(h map[string]any) map[string]any { h["jwk"].(map[string]string)["d"] = "AAAA"; return h },
			wantError: "invalid_dpop_proof",
		},
		{
			name:      "a proof carrying no access-token hash",
			claims:    func(c map[string]any) map[string]any { delete(c, "ath"); return c },
			wantError: "invalid_dpop_proof",
		},
		{
			name:      "a proof naming another token's hash",
			claims:    func(c map[string]any) map[string]any { c["ath"] = "bm90LXRoZS1zYW1lLXRva2Vu"; return c },
			wantError: "invalid_dpop_proof",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			header, claims := signer.header(), good()
			if tc.header != nil {
				header = tc.header(header)
			}
			if tc.claims != nil {
				claims = tc.claims(claims)
			}
			verdict := ffiDPoPPolicy{}.ValidateDPoPProof(signer.sign(t, header, claims), expect)
			if verdict.Proof != nil {
				t.Fatalf("%s was accepted", tc.name)
			}
			if verdict.Deny.Error != tc.wantError {
				t.Errorf("error = %q, want %q (%s)", verdict.Deny.Error, tc.wantError, verdict.Deny.Description)
			}
		})
	}
}

// ── End to end through the resource server ──────────────────────────────────

// crossBinaryIssuerKey mints access tokens the way the nest's authorization
// server does, with its own independent JOSE encoding — the same "independent
// producer" rule crossBinarySigner follows.
type crossBinaryIssuerKey struct{ key *ecdsa.PrivateKey }

func (k crossBinaryIssuerKey) feed() atprotopds.NestIssuerJWK {
	var x, y [32]byte
	k.key.X.FillBytes(x[:])
	k.key.Y.FillBytes(y[:])
	return atprotopds.NestIssuerJWK{
		Kid: "cross-binary-issuer-key",
		X:   base64.RawURLEncoding.EncodeToString(x[:]),
		Y:   base64.RawURLEncoding.EncodeToString(y[:]),
	}
}

func (k crossBinaryIssuerKey) sign(t *testing.T, claims map[string]any) string {
	t.Helper()
	seg := func(v any) string {
		raw, err := json.Marshal(v)
		if err != nil {
			t.Fatalf("marshal segment: %v", err)
		}
		return base64.RawURLEncoding.EncodeToString(raw)
	}
	signingInput := seg(map[string]any{"alg": "ES256", "typ": "at+jwt", "kid": "cross-binary-issuer-key"}) +
		"." + seg(claims)
	digest := sha256.Sum256([]byte(signingInput))
	r, sv, err := ecdsa.Sign(rand.Reader, k.key, digest[:])
	if err != nil {
		t.Fatalf("sign access token: %v", err)
	}
	var sig [64]byte
	r.FillBytes(sig[:32])
	sv.FillBytes(sig[32:])
	return signingInput + "." + base64.RawURLEncoding.EncodeToString(sig[:])
}

// **The whole resource-server path, both binaries, no fakes anywhere.**
//
// A nest-shaped access token, bound to a client key by an independently
// computed RFC-7638 thumbprint, is presented on an XRPC call through the real
// frame. The client discovers its nonce the way a real one does — by being
// told — and its proof is then validated by real Rust (including the `ath`
// rule), verified by real Go crypto, and thumbprint-compared against the
// token's `cnf.jkt`. A replay of that proof is refused by the replay set after
// the Rust validator has already accepted it.
func TestANestShapedTokenSurvivesTheWholeResourceServerPath(t *testing.T) {
	issuerPriv, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	issuer := crossBinaryIssuerKey{key: issuerPriv}
	nestIssuer := faunaAtproto.OauthIssuer(crossBinaryApex)

	minter := atprotopds.NewTokenMinter(atprotopds.StaticSecret("0123456789abcdef0123456789abcdef"),
		faunaAtproto.AtprotoPdsServiceDid(crossBinaryApex), nil)
	s := atprotopds.NewServer(nil, minter, nil, nil, nil)
	s.EnableOAuthResourceServer(ffiDPoPPolicy{}, crossBinaryPDSOrigin())
	if err := s.SetNestIssuerKeys(nestIssuer, []atprotopds.NestIssuerJWK{issuer.feed()}); err != nil {
		t.Fatalf("SetNestIssuerKeys: %v", err)
	}

	client := newCrossBinarySigner(t)
	now := time.Now()
	token := issuer.sign(t, map[string]any{
		"iss":         nestIssuer,
		"sub":         "did:plc:crossbinaryaccount",
		"aud":         faunaAtproto.AtprotoPdsServiceDid(crossBinaryApex),
		"scope":       "atproto",
		"iat":         now.Unix(),
		"exp":         now.Add(15 * time.Minute).Unix(),
		"jti":         "cross-binary-access-1",
		"cnf":         map[string]string{"jkt": client.jkt(t)},
		"sid":         base64.RawURLEncoding.EncodeToString([]byte("grant-family")),
		"fauna_actor": hex.EncodeToString(bytes.Repeat([]byte{0x42}, 32)),
	})
	tokenHash := sha256.Sum256([]byte(token))

	allowAll := func(*http.Request, *xrpc.Route, *xrpc.Caller) *xrpc.Error { return nil }
	frame := xrpc.NewServer(nil, s.OAuthTokenVerifier(), allowAll, nil, nil)
	frame.Register(xrpc.Route{
		NSID: "com.atproto.server.getSession", Method: http.MethodGet, Auth: xrpc.OAuthSession,
		Handle: func(w http.ResponseWriter, _ *http.Request, c *xrpc.Caller) {
			xrpc.WriteJSON(w, map[string]any{"did": c.DID})
		}})

	proof := func(jti, nonce string) string {
		claims := map[string]any{
			"jti": jti,
			"htm": http.MethodGet,
			"htu": crossBinaryPDSOrigin() + crossBinaryPath,
			"iat": time.Now().Unix(),
			"ath": base64.RawURLEncoding.EncodeToString(tokenHash[:]),
		}
		if nonce != "" {
			claims["nonce"] = nonce
		}
		return client.sign(t, client.header(), claims)
	}
	call := func(p string) *httptest.ResponseRecorder {
		req := httptest.NewRequest(http.MethodGet, crossBinaryPath, nil)
		req.Header.Set("Authorization", "DPoP "+token)
		req.Header.Set("DPoP", p)
		rec := httptest.NewRecorder()
		frame.ServeHTTP(rec, req)
		return rec
	}

	// First contact: a nonce-less proof. Real Rust answers `use_dpop_nonce`,
	// and the refusal carries the nonce that makes the retry possible.
	first := call(proof("first-contact", ""))
	if first.Code != http.StatusUnauthorized {
		t.Fatalf("a nonce-less proof returned %d, want 401: %s", first.Code, first.Body.String())
	}
	if got, want := first.Header().Get("WWW-Authenticate"), `DPoP error="use_dpop_nonce"`; got != want {
		t.Errorf("challenge = %q, want %q", got, want)
	}
	nonce := first.Header().Get("DPoP-Nonce")
	if nonce == "" {
		t.Fatal("the refusal carried no DPoP-Nonce, so a real client could never proceed")
	}

	good := proof("first-call", nonce)
	if rec := call(good); rec.Code != http.StatusOK {
		t.Fatalf("the DPoP-bound call returned %d, want 200: %s", rec.Code, rec.Body.String())
	}
	// The same proof twice is a replay, refused after the real Rust validator
	// has already accepted it — so the replay set, not the policy, caught it.
	if again := call(good); again.Code != http.StatusUnauthorized {
		t.Fatalf("a replayed proof returned %d, want 401", again.Code)
	}
	// …and a fresh proof under the same nonce still works, so the replay set
	// keys on the proof and not on the nonce.
	if fresh := call(proof("second-call", nonce)); fresh.Code != http.StatusOK {
		t.Fatalf("a fresh proof under the same nonce returned %d, want 200: %s",
			fresh.Code, fresh.Body.String())
	}
}

// **The `ath` requirement really crosses the FFI, and it is REAL Rust
// deciding** (F4 slice 7).
//
// The Go-side tests can only assert which expectations were assembled — the
// fake module they run against implements none of the claim rules. A dropped
// `expected_ath` fails OPEN in the direction that costs the most: a
// resource-server proof would then be accepted with no `ath` at all, and the
// binding between a proof and the token it accompanies — the thing that stops a
// captured proof being replayed alongside a different token — would be gone
// with every test still green.
func TestTheAthRequirementCrossesTheFfi(t *testing.T) {
	signer := newCrossBinarySigner(t)
	policy := ffiDPoPPolicy{}
	expect := crossBinaryExpectations()

	claims := func(ath string) map[string]any {
		c := map[string]any{
			"jti":   "cross-binary-ath-1",
			"htm":   expect.HTM,
			"htu":   expect.HTU,
			"iat":   expect.NowUnix,
			"nonce": "a-nonce-the-server-issued",
		}
		if ath != "" {
			c["ath"] = ath
		}
		return c
	}

	if v := policy.ValidateDPoPProof(signer.sign(t, signer.header(), claims(crossBinaryAth)), expect); v.Proof == nil {
		t.Errorf("a resource-server proof carrying the right ath was refused: %+v", v.Deny)
	}
	if v := policy.ValidateDPoPProof(signer.sign(t, signer.header(), claims("")), expect); v.Proof != nil {
		t.Error("a resource-server request accepted a proof with NO ath — the " +
			"expectation did not reach Rust")
	}
	other := "bm90LXRoZS1zYW1lLXRva2VuLWhhc2gtYXQtYWxsLW5vcGU"
	wrong := expect
	wrong.ExpectedAth = &other
	if v := policy.ValidateDPoPProof(signer.sign(t, signer.header(), claims(crossBinaryAth)), wrong); v.Proof != nil {
		t.Error("a proof whose ath names a different token was accepted")
	}
}
