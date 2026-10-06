package wsrpc

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"encoding/hex"
	"errors"
	"fmt"
	"net/http"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// authFakeCaller is a fake Caller that mimics nest's pre-identity
// fauna.auth.{nest_handshake,challenge,verify} kinds
// (bins/fauna-nest/src/auth_handlers.rs) over the anonymous connection: it
// proves a nest identity over the caller's nonce (the tagged channel binding
// a real nest signs), returns a fixed nonce for challenge, verifies the
// Ed25519 signature over the domain-tagged, nest-bound
// ChallengeVerifySignedMessage(actor_id, nonce, nest_id) for verify — the
// same bytes nest checks, and only when `nest_id` names the identity it proved
// — and mints a token. It records call counts + the last request bodies so
// tests can assert both the ceremony outcome and the on-wire DAG-CBOR shape
// — no WS server.
//
// `nonce` is fixed (not random) so a test can reason about the signed message.
type authFakeCaller struct {
	pubKey   ed25519.PublicKey
	nonce    [32]byte
	token    string
	tokenTTL time.Duration
	// verifyErr, when non-nil, is returned from the verify step (e.g. to
	// simulate a fauna.auth.not_registered rejection) instead of minting a
	// token. The signature is still checked first.
	verifyErr error
	// forgeBinding makes the nest handshake CLAIM an identity the fake does
	// not hold the key for — a relaying box presenting someone else's identity
	// over a proof it cannot produce.
	forgeBinding bool

	challengeCnt atomic.Int64
	verifyCnt    atomic.Int64

	mu          sync.Mutex
	gotChalBody []byte // canonical CBOR of the challenge request body
	gotVerBody  []byte // canonical CBOR of the verify request body

	nestOnce sync.Once
	nestPub  ed25519.PublicKey
	nestPriv ed25519.PrivateKey
}

// nestIdentity is the fake nest's deployment key, minted lazily so the inline
// `&authFakeCaller{...}` literals every test builds need no constructor.
func (f *authFakeCaller) nestIdentity() (ed25519.PublicKey, ed25519.PrivateKey) {
	f.nestOnce.Do(func() {
		pub, priv, err := ed25519.GenerateKey(nil)
		if err != nil {
			panic(err)
		}
		f.nestPub, f.nestPriv = pub, priv
	})
	return f.nestPub, f.nestPriv
}

func (f *authFakeCaller) Call(_ context.Context, method string, body, reply any) error {
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	switch method {
	case methodAuthNestHandshake:
		var req nestHandshakeRequest
		if err := cbor.Unmarshal(enc, &req); err != nil {
			return err
		}
		nestPub, nestPriv := f.nestIdentity()
		claimed := nestPub
		if f.forgeBinding {
			claimed, _, _ = ed25519.GenerateKey(nil)
		}
		// A plaintext fake: no served SPKI, the tagged message is the nonce
		// alone (exactly `build_identity_binding`'s cert-less arm).
		tagged := ed25519.Sign(nestPriv, CertBindingSignedMessage(nil, req.ClientNonce))
		repBytes, err := dagcbor.Marshal(nestHandshakeReply{CertBinding: &certBindingWire{
			NestActorID: hex.EncodeToString(claimed),
			SpkiSha256:  []byte{},
			TaggedSig:   tagged,
		}})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(repBytes, reply)
	case methodAuthChallenge:
		f.challengeCnt.Add(1)
		f.mu.Lock()
		f.gotChalBody = enc
		f.mu.Unlock()
		repBytes, err := dagcbor.Marshal(authChallengeReply{Nonce: hex.EncodeToString(f.nonce[:])})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(repBytes, reply)
	case methodAuthVerify:
		f.verifyCnt.Add(1)
		f.mu.Lock()
		f.gotVerBody = enc
		f.mu.Unlock()
		var req authVerifyRequest
		if err := cbor.Unmarshal(enc, &req); err != nil {
			return err
		}
		actor, _ := hex.DecodeString(req.ActorID)
		nonceBytes, _ := hex.DecodeString(req.Nonce)
		sig, _ := hex.DecodeString(req.Signature)
		nestPub, _ := f.nestIdentity()
		// The nest binding, exactly as `auth_core::verify_core` gates it: a
		// blob naming another nest is refused before any signature work, and
		// the message is built over the nest's OWN identity.
		if req.NestID != hex.EncodeToString(nestPub) {
			return &ServerError{Payload: mustRpcErrorPayload("fauna.auth.invalid_request")}
		}
		// Verify the DOMAIN-TAGGED, nest-bound bytes, exactly as nest's
		// `auth_core` verifier does — the fake server is only a useful
		// stand-in while it refuses what the real one refuses.
		msg := ChallengeVerifySignedMessage(actor, nonceBytes, nestPub)
		if !ed25519.Verify(f.pubKey, msg, sig) {
			return &ServerError{Payload: mustRpcErrorPayload("fauna.auth.signature_failed")}
		}
		if f.verifyErr != nil {
			return f.verifyErr
		}
		ttl := f.tokenTTL
		if ttl == 0 {
			ttl = time.Hour
		}
		repBytes, err := dagcbor.Marshal(authVerifyReply{
			Token:     f.token,
			ExpiresAt: time.Now().Add(ttl).Unix(),
		})
		if err != nil {
			return err
		}
		return cbor.Unmarshal(repBytes, reply)
	default:
		return fmt.Errorf("authFakeCaller: unexpected method %q", method)
	}
}

// mustRpcErrorPayload encodes a minimal `{code: …}` RpcError-shaped CBOR
// payload — the body a real nest carries on an ok=false reply, so RpcErrorCode
// can decode it back out of the wrapping *ServerError.
func mustRpcErrorPayload(code string) []byte {
	b, err := dagcbor.Marshal(struct {
		Code string `cbor:"code"`
	}{Code: code})
	if err != nil {
		panic(err)
	}
	return b
}

func newTestKey(t *testing.T) (ed25519.PublicKey, ed25519.PrivateKey) {
	t.Helper()
	pub, priv, err := ed25519.GenerateKey(nil)
	if err != nil {
		t.Fatalf("gen key: %v", err)
	}
	return pub, priv
}

// TestAuthClientCeremonyOverCaller drives the challenge/verify ceremony over a
// fake Caller and asserts (a) the token is returned with an absolute expiry,
// (b) exactly one challenge + one verify call, (c) the on-wire shape — actor_id
// / nonce / signature ride as CBOR text strings (hex), matching the Rust wire
// types (libs/fauna-protocol/src/auth.rs), and (d) the signed bytes validate
// (the fake returns a token only if ed25519.Verify(actor_id||nonce) passes, so
// reaching success proves the signed-message construction).
func TestAuthClientCeremonyOverCaller(t *testing.T) {
	t.Parallel()
	pub, priv := newTestKey(t)
	fake := &authFakeCaller{pubKey: pub, nonce: [32]byte{0x11}, token: "tok-XYZ", tokenTTL: time.Hour}
	auth := NewAuthClient(http.DefaultClient, "http://nest.invalid", pub, priv)

	tok, exp, err := auth.acquireOverCaller(context.Background(), fake)
	if err != nil {
		t.Fatalf("acquireOverCaller: %v", err)
	}
	if tok != "tok-XYZ" {
		t.Errorf("token = %q, want tok-XYZ", tok)
	}
	if !exp.After(time.Now()) {
		t.Errorf("expiry %v is not in the future", exp)
	}
	if c := fake.challengeCnt.Load(); c != 1 {
		t.Errorf("challenge calls = %d, want 1", c)
	}
	if v := fake.verifyCnt.Load(); v != 1 {
		t.Errorf("verify calls = %d, want 1", v)
	}

	// Challenge body: actor_id is a CBOR text string (hex), not a byte string —
	// the Rust ChallengeRequest.actor_id is a String.
	var chMap map[string]any
	if err := cbor.Unmarshal(fake.gotChalBody, &chMap); err != nil {
		t.Fatalf("decode challenge body: %v", err)
	}
	if got, ok := chMap["actor_id"].(string); !ok || got != hex.EncodeToString(pub) {
		t.Errorf("challenge actor_id = %v (%T), want hex string %s",
			chMap["actor_id"], chMap["actor_id"], hex.EncodeToString(pub))
	}

	// Verify body: actor_id / nonce / signature / nest_id are all hex text strings.
	var verMap map[string]any
	if err := cbor.Unmarshal(fake.gotVerBody, &verMap); err != nil {
		t.Fatalf("decode verify body: %v", err)
	}
	for _, k := range []string{"actor_id", "nonce", "signature", "nest_id"} {
		if _, ok := verMap[k].(string); !ok {
			t.Errorf("verify body field %q = %v (%T), want hex string", k, verMap[k], verMap[k])
		}
	}
}

// TestAuthClientAcquiresFreshToken — happy path: a brand-new client returns the
// minted token on its first Token() call. The acquireToken seam routes the real
// ceremony over the fake Caller (no WS server), mirroring the `now` clock seam.
func TestAuthClientAcquiresFreshToken(t *testing.T) {
	t.Parallel()
	pub, priv := newTestKey(t)
	fake := &authFakeCaller{pubKey: pub, nonce: [32]byte{0xAB}, token: "test-token-XYZ", tokenTTL: time.Hour}
	auth := NewAuthClient(http.DefaultClient, "http://nest.invalid", pub, priv)
	auth.acquireToken = func(ctx context.Context) (string, time.Time, error) {
		return auth.acquireOverCaller(ctx, fake)
	}

	got, err := auth.Token(context.Background())
	if err != nil {
		t.Fatalf("Token: %v", err)
	}
	if got != "test-token-XYZ" {
		t.Errorf("Token = %q, want test-token-XYZ", got)
	}
	if c := fake.challengeCnt.Load(); c != 1 {
		t.Errorf("challenge calls = %d, want 1", c)
	}
	if v := fake.verifyCnt.Load(); v != 1 {
		t.Errorf("verify calls = %d, want 1", v)
	}
}

// TestAuthClientReusesUnexpiredToken — two consecutive Token() calls share the
// cached token; no second round-trip.
func TestAuthClientReusesUnexpiredToken(t *testing.T) {
	t.Parallel()
	pub, priv := newTestKey(t)
	fake := &authFakeCaller{pubKey: pub, nonce: [32]byte{0xCD}, token: "token-1", tokenTTL: time.Hour}
	auth := NewAuthClient(http.DefaultClient, "http://nest.invalid", pub, priv)
	auth.acquireToken = func(ctx context.Context) (string, time.Time, error) {
		return auth.acquireOverCaller(ctx, fake)
	}

	t1, err := auth.Token(context.Background())
	if err != nil {
		t.Fatalf("Token #1: %v", err)
	}
	t2, err := auth.Token(context.Background())
	if err != nil {
		t.Fatalf("Token #2: %v", err)
	}
	if t1 != t2 {
		t.Errorf("Token() returned different tokens on consecutive calls: %q vs %q", t1, t2)
	}
	if c := fake.challengeCnt.Load(); c != 1 {
		t.Errorf("challenge round-trips = %d, want 1 (second Token() should hit cache)", c)
	}
}

// TestAuthClientRefreshesOnExpiry — fake-clock-driven: the cached token is set
// to expire within refreshLeadTime, and the next Token() call must re-acquire.
func TestAuthClientRefreshesOnExpiry(t *testing.T) {
	t.Parallel()
	pub, priv := newTestKey(t)
	// Issue a token with a 60-second TTL; then advance time to within
	// refreshLeadTime (30s) of expiry and the next Token() must re-acquire.
	fake := &authFakeCaller{pubKey: pub, nonce: [32]byte{0xEF}, token: "first", tokenTTL: 60 * time.Second}
	auth := NewAuthClient(http.DefaultClient, "http://nest.invalid", pub, priv)
	auth.acquireToken = func(ctx context.Context) (string, time.Time, error) {
		return auth.acquireOverCaller(ctx, fake)
	}

	// Drive a fake clock so we can move into the refresh window without
	// sleeping. Start at "real now" so the fake's time.Unix(expires_at) (also
	// real-now-relative) is comfortably in the future.
	clock := &fakeClock{now: time.Now()}
	auth.now = clock.Now

	if _, err := auth.Token(context.Background()); err != nil {
		t.Fatalf("Token #1: %v", err)
	}
	if v := fake.verifyCnt.Load(); v != 1 {
		t.Errorf("verify calls after first Token: %d, want 1", v)
	}

	// Advance the test clock 45s — the cached token (60s TTL) now has 15s left,
	// which is under refreshLeadTime (30s), so Token() must re-acquire.
	fake.token = "second"
	clock.advance(45 * time.Second)

	got, err := auth.Token(context.Background())
	if err != nil {
		t.Fatalf("Token #2: %v", err)
	}
	if got != "second" {
		t.Errorf("Token after expiry = %q, want second", got)
	}
	if v := fake.verifyCnt.Load(); v != 2 {
		t.Errorf("verify calls after second Token: %d, want 2", v)
	}
}

// TestChallengeVerifySignedMessageIsTagged pins the EXACT bytes a
// `fauna.auth.verify` signature covers against a literal, because this side of
// the contract is hand-rolled Go and the other side is Rust
// (`fauna_protocol::auth::challenge_verify_signed_message` over the
// `AUTH_VERIFY_V2` tag) — no compiler, no generated binding, and no Rust-side
// grep connects them. Only a byte-level pin does.
//
// This replaces a test that signed its own hand-built `actor_id ‖ nonce` and
// verified it with the same hand-built bytes: a tautology that asserted
// ed25519 works rather than that the bridge signs what the nest accepts. It
// therefore stayed green through the 2026-08-17 tagged-only sweep, which left
// this signer untagged and took every bridge role off the air —
// `fauna.auth.signature_failed` on every bearer acquisition, so no MTA, MDA or
// CalDAV bridge could dial nest at all.
//
// If the Rust tag ever changes, this test must be updated in the same change.
func TestChallengeVerifySignedMessageIsTagged(t *testing.T) {
	t.Parallel()
	actor := make([]byte, 32)
	nonce := make([]byte, 32)
	nest := make([]byte, 32)
	for i := range actor {
		actor[i] = byte(i)
		nonce[i] = byte(0x40 + i)
		nest[i] = byte(0x80 + i)
	}

	got := ChallengeVerifySignedMessage(actor, nonce, nest)

	want := append([]byte("fauna.auth.verify.v2\x00"), append(append(append([]byte{}, actor...), nonce...), nest...)...)
	if !bytes.Equal(got, want) {
		t.Fatalf("signed message is not the tagged, nest-bound form nest verifies:\n got %x\nwant %x", got, want)
	}
	// The tag must lead — an untagged `actor_id ‖ nonce` is exactly what the
	// nest refuses outright now that the transition path is deleted.
	if !bytes.HasPrefix(got, []byte("fauna.auth.verify.v2\x00")) {
		t.Fatal("the domain tag must prefix the signed message")
	}
	if bytes.Equal(got, append(append([]byte{}, actor...), nonce...)) {
		t.Fatal("signed message is the retired UNTAGGED form; nest answers signature_failed")
	}
	// The nest identity must be bound in: the same bytes for another nest
	// differ (`login.md` § Binding the nest — the retired unbound form was one
	// signature valid at every nest).
	other := append([]byte{}, nest...)
	other[0] ^= 0xff
	if bytes.Equal(got, ChallengeVerifySignedMessage(actor, nonce, other)) {
		t.Fatal("the signed message does not bind the nest identity")
	}
}

// TestAuthClientRefusesAForgedNestBinding — a box that CLAIMS an identity it
// cannot sign for (a relay presenting the real nest's identity over its own
// connection, without the real nest's key) gets no login signature at all:
// the ceremony stops at the identity read, before the challenge is even
// requested.
func TestAuthClientRefusesAForgedNestBinding(t *testing.T) {
	t.Parallel()
	pub, priv := newTestKey(t)
	fake := &authFakeCaller{pubKey: pub, nonce: [32]byte{0x33}, token: "tok", tokenTTL: time.Hour, forgeBinding: true}
	auth := NewAuthClient(http.DefaultClient, "http://nest.invalid", pub, priv)

	_, _, err := auth.acquireOverCaller(context.Background(), fake)
	if err == nil {
		t.Fatal("acquire succeeded against a forged nest binding; want an error")
	}
	if c := fake.challengeCnt.Load(); c != 0 {
		t.Errorf("challenge calls = %d, want 0 — nothing is signed for an unproven identity", c)
	}
	if v := fake.verifyCnt.Load(); v != 0 {
		t.Errorf("verify calls = %d, want 0", v)
	}
}

// TestAuthClientSignsTheTaggedMessage drives the PRODUCTION signing path end to
// end over a fake Caller and verifies the signature the client actually put on
// the wire against the tagged bytes — the assertion the tautology above could
// not make, since it never called the code under test.
func TestAuthClientSignsTheTaggedMessage(t *testing.T) {
	t.Parallel()
	pub, priv := newTestKey(t)
	f := &authFakeCaller{pubKey: pub, nonce: [32]byte{0x5A}, token: "tok", tokenTTL: time.Hour}
	a := NewAuthClient(http.DefaultClient, "http://nest.invalid", pub, priv)

	if _, _, err := a.acquireOverCaller(context.Background(), f); err != nil {
		t.Fatalf("acquire over the production ceremony failed: %v", err)
	}

	var req authVerifyRequest
	f.mu.Lock()
	body := f.gotVerBody
	f.mu.Unlock()
	if err := cbor.Unmarshal(body, &req); err != nil {
		t.Fatalf("decode the verify request the client sent: %v", err)
	}
	sig, err := hex.DecodeString(req.Signature)
	if err != nil {
		t.Fatalf("decode signature hex: %v", err)
	}
	nonceBytes, err := hex.DecodeString(req.Nonce)
	if err != nil {
		t.Fatalf("decode nonce hex: %v", err)
	}
	nestPub, _ := f.nestIdentity()
	if req.NestID != hex.EncodeToString(nestPub) {
		t.Fatalf("the verify names nest_id %s, want the identity the handshake proved %x", req.NestID, nestPub)
	}
	if !ed25519.Verify(pub, ChallengeVerifySignedMessage(pub, nonceBytes, nestPub), sig) {
		t.Fatal("the signature the client sent does not cover the tagged, nest-bound message nest verifies")
	}
}

// TestAuthClientSurfaceServerError — a fauna.auth.verify rejection (ok=false
// reply, here fauna.auth.not_registered) surfaces as an error that wraps
// ErrServerError and whose RpcError code is recoverable, so admins see the
// real rejection reason (the deauthorized-bridge crash-loop signal).
func TestAuthClientSurfaceServerError(t *testing.T) {
	t.Parallel()
	pub, priv := newTestKey(t)
	fake := &authFakeCaller{
		pubKey:    pub,
		nonce:     [32]byte{0x01},
		verifyErr: &ServerError{Payload: mustRpcErrorPayload("fauna.auth.not_registered")},
	}
	auth := NewAuthClient(http.DefaultClient, "http://nest.invalid", pub, priv)

	_, _, err := auth.acquireOverCaller(context.Background(), fake)
	if err == nil {
		t.Fatal("acquireOverCaller succeeded against a verify ServerError; want error")
	}
	if !errors.Is(err, ErrServerError) {
		t.Errorf("err = %v, want it to wrap ErrServerError", err)
	}
	if code, ok := RpcErrorCode(err); !ok || code != "fauna.auth.not_registered" {
		t.Errorf("RpcErrorCode = (%q, %v), want (fauna.auth.not_registered, true)", code, ok)
	}
}

// fakeClock is a minimal monotonic clock for tests.
type fakeClock struct {
	mu  sync.Mutex
	now time.Time
}

func (c *fakeClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *fakeClock) advance(d time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.now = c.now.Add(d)
}
