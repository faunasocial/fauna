// auth.go implements the WS-RPC challenge/verify bearer-token flow that
// the bridge uses to obtain a per-connection auth token before the
// authenticated WebSocket Dial.
//
// Wire flow — the `fauna.auth.{challenge,verify}` pre-identity WS-RPC kinds
// (bins/fauna-nest/src/auth_handlers.rs; wire types
// libs/fauna-protocol/src/auth.rs), run over the **anonymous** WS connection
// (GET /api/v1/ws, no bearer) — the same connection the cold-boot
// self-enrollment uses (enroll.go / DialAnonymous). This replaces the former
// HTTP POSTs to /api/v1/auth/{challenge,verify} (deleted in the
// WS-RPC-everywhere rip-out); the Ed25519 ceremony is preserved exactly, only
// the transport moves from HTTP-JSON to DAG-CBOR WS-RPC. actor_id / nonce /
// signature ride as **hex strings** on the wire (matching the Rust wire types),
// so the signed-message construction below is unchanged from the HTTP twin:
//
//  0. fauna.auth.nest_handshake {client_nonce: <32 bytes>}
//     → {cert_binding: {nest_actor_id, spki_sha256, sig, tagged_sig}} — the
//     identity of the nest at the far end of THIS connection, possession-
//     verified over the fresh nonce (readLoginBinding; `login.md` § Binding
//     the nest). Every login signature names it, and the nest refuses a
//     signature naming any other nest — so a nest the bridge dials cannot
//     relay the signature and mint the bridge a bearer elsewhere.
//
//  1. fauna.auth.challenge {actor_id: <hex>}
//     → {nonce: <32-byte hex>, expires_in, expires_at}
//
//  2. Sign ChallengeVerifySignedMessage(actor_id, nonce, nest_id) — the
//     domain-tagged `fauna.auth.verify.v2\0 ‖ actor_id ‖ nonce ‖ nest_id` —
//     with the bridge's Ed25519 signing key.
//
//  3. fauna.auth.verify {actor_id: <hex>, nonce: <hex>, signature: <64-byte sig hex>, nest_id: <hex>}
//     → {token: <opaque>, token_id, handle, domain, tier, expires_at}
//
// AuthClient caches the resulting bearer token and refreshes it on the
// next Token() call within 30s of expiry. The "30s" margin gives the WS
// dial enough headroom to negotiate before the token would actually
// reach the server expired (nest's clock and ours may drift a little;
// 30s is the smallest comfortable buffer).
//
// Each acquire opens its own short-lived anonymous WS connection, runs the two
// kinds, and closes it — mirroring EnrollAndAwaitApproval. The anonymous
// connector carries no reconnect supervisor, so a fresh connection per acquire
// (rather than one cached across the process lifetime) sidesteps mid-wait
// connection-rot for what is a once-per-~50min round-trip.
//
// Thread-safety: Token() holds an internal mutex over the cache check
// and the acquire round-trip. Concurrent callers serialize and share the
// fetched token (no thundering-herd).
//
// Mid-TTL auth failure — a nest restart forgets every bearer:
//
// Nest's bearer tokens live in an in-memory store
// (`bins/fauna-nest/src/token_store.rs`), so a nest restart invalidates
// every outstanding token mid-TTL, and the next WS upgrade answers HTTP 401
// with no close code. This note used to argue that needed no in-process
// recovery, because a dropped WS exited the process and s6/systemd restarted
// it with an empty cache. That stopped holding once the bridges redial
// in-process (`NewReconnectingClient`): the redial re-presented the cached
// bearer until it neared expiry, so a nest restart locked a bridge out for up
// to a token lifetime (measured on an e2e sweep: 82 redials over 44 minutes).
//
// So [Dial] follows the client rule (`transport-connection.md` § Connection
// lifecycle → Upgrade-time auth rejection): on a 401 at the upgrade it calls
// [AuthClient.Invalidate], re-mints once and retries once, and a fresh bearer
// still refused goes to the caller's backoff — never a refresh→401 busy loop.
//
// Genuine revocation (admin deleted / un-approved the service-user row) still
// fails at the re-mint's challenge/verify (a `fauna.auth.not_registered`
// RpcError), which reaches the caller like any other dial failure.
package wsrpc

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"strings"
	"sync"
	"time"
)

// WS-RPC kinds for the pre-identity auth-bootstrap ceremony. Unexported: the
// AuthClient is the only caller (the challenge/verify pair is an internal detail
// of bearer acquisition, not part of the broad fauna.bridges.* method surface
// in methods.go). Mirrors bins/fauna-nest/src/auth_handlers.rs.
const (
	methodAuthNestHandshake = "fauna.auth.nest_handshake"
	methodAuthChallenge     = "fauna.auth.challenge"
	methodAuthVerify        = "fauna.auth.verify"
)

// nestHandshakeRequest mirrors libs/fauna-protocol/src/auth.rs:NestHandshakeRequest
// — the fresh client nonce the nest signs its channel binding over (a CBOR
// byte string on the wire).
type nestHandshakeRequest struct {
	ClientNonce []byte `cbor:"client_nonce"`
}

// certBindingWire mirrors auth.rs:CertBinding — the nest's channel-binding
// proof. `tagged_sig` is the only signature a binding carries (the untagged
// `sig` compat half left the wire 2026-09-24 with the compat-remnant sweep).
type certBindingWire struct {
	NestActorID string `cbor:"nest_actor_id"`
	SpkiSha256  []byte `cbor:"spki_sha256"`
	TaggedSig   []byte `cbor:"tagged_sig"`
}

// nestHandshakeReply mirrors auth.rs:NestHandshakeReply; `cert_binding` is
// absent from a keyless nest.
type nestHandshakeReply struct {
	CertBinding *certBindingWire `cbor:"cert_binding"`
}

// authChallengeRequest mirrors libs/fauna-protocol/src/auth.rs:ChallengeRequest.
// actor_id is a hex String on the wire (the identity-reference convention —
// only opaque payloads ride as CBOR byte strings), so it matches the HTTP
// twin's JSON shape byte-for-byte in semantics.
type authChallengeRequest struct {
	ActorID string `cbor:"actor_id"`
}

// authChallengeReply mirrors ChallengeReply. The wire also carries
// `expires_in`/`expires_at`, but the bridge ignores them: the nonce is consumed
// immediately by the verify step below.
type authChallengeReply struct {
	Nonce string `cbor:"nonce"`
}

// authVerifyRequest mirrors VerifyRequest — all four fields are hex Strings.
// `nest_id` is the identity the signature binds (readLoginBinding); the nest
// requires it to be its own.
type authVerifyRequest struct {
	ActorID   string `cbor:"actor_id"`
	Nonce     string `cbor:"nonce"`
	Signature string `cbor:"signature"`
	NestID    string `cbor:"nest_id"`
}

// authVerifyReply mirrors VerifyReply. The bridge needs only the bearer token +
// its absolute expiry; the wire also carries `token_id`/`handle`/`domain`/`tier`
// (relevant to a UI client, not a headless service user) which the CBOR decoder
// ignores.
type authVerifyReply struct {
	Token     string `cbor:"token"`
	ExpiresAt int64  `cbor:"expires_at"`
}

// refreshLeadTime is the slack we leave between "the cached token
// expires" and "we proactively re-acquire." 30s is comfortably larger
// than typical clock skew between two networked machines, and well
// within the 1h TTL nest issues — so the renewal cost is negligible
// (one re-acquire per ~3600s ÷ ~30s ≈ 1 round-trip every 50min).
const refreshLeadTime = 30 * time.Second

// AuthClient acquires and refreshes bearer tokens against nest's
// fauna.auth.{challenge,verify} pre-identity WS-RPC kinds.
//
// AuthClient is goroutine-safe.
type AuthClient struct {
	httpClient  *http.Client
	nestBaseURL string // e.g., "https://nest.example.com" (no /api/v1/... suffix)
	actorID     []byte // 32-byte Ed25519 pubkey (the service-user actor_id)
	signKey     ed25519.PrivateKey
	logger      *slog.Logger

	// now is a test seam — production uses time.Now. acquireToken is the
	// token-minting strategy: production runs the ceremony over a freshly
	// dialed anonymous WS (acquireViaAnonymousWS); tests swap it to drive
	// Token()'s caching/refresh logic over a fake Caller without a WS server,
	// exactly as `now` swaps the clock. Both are set in NewAuthClient before
	// any goroutine touches the client, so neither is guarded by the mutex.
	now          func() time.Time
	acquireToken func(ctx context.Context) (string, time.Time, error)

	mu      sync.Mutex
	token   string
	expires time.Time
}

// NewAuthClient constructs an AuthClient.
//
// `nestBaseURL` is the nest's external URL up to but NOT including the
// `/api/v1/...` path (e.g., `https://nest.example.com`). The anonymous WS
// endpoint (`/api/v1/ws`) is appended internally by DialAnonymous. `actorID`
// must be a 32-byte Ed25519 public key. `signKey` must be the matching 64-byte
// Ed25519 private key (the kind returned by keypair.Keyfile.SigningKey).
//
// Passing a nil http.Client falls back to http.DefaultClient. Production should
// pass an explicit client with a TLS config + sensible timeouts — it is
// forwarded to the anonymous WS dial (websocket.DialOptions.HTTPClient) so the
// challenge/verify round-trips ride the same TLS as the authenticated Dial.
func NewAuthClient(httpClient *http.Client, nestBaseURL string, actorID []byte, signKey ed25519.PrivateKey) *AuthClient {
	if httpClient == nil {
		httpClient = http.DefaultClient
	}
	a := &AuthClient{
		httpClient:  httpClient,
		nestBaseURL: strings.TrimRight(nestBaseURL, "/"),
		actorID:     actorID,
		signKey:     signKey,
		logger:      slog.Default(),
		now:         time.Now,
	}
	a.acquireToken = a.acquireViaAnonymousWS
	return a
}

// Token returns a valid bearer token, refreshing if the cache is empty
// or the cached token is within refreshLeadTime of expiry.
//
// The returned token is suitable for the `bearer.<token>` subprotocol
// value in the WebSocket dial.
func (a *AuthClient) Token(ctx context.Context) (string, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.token != "" && a.now().Add(refreshLeadTime).Before(a.expires) {
		return a.token, nil
	}
	// Cache miss or near-expiry: re-acquire under the mutex so
	// concurrent callers share the result.
	token, expires, err := a.acquireToken(ctx)
	if err != nil {
		return "", err
	}
	a.token = token
	a.expires = expires
	return token, nil
}

// Invalidate drops the cached bearer, so the next [AuthClient.Token] mints a
// fresh one. [Dial] calls it when the nest refuses the bearer at the WS
// upgrade (HTTP 401) — a restarted nest's in-memory token store no longer
// holds it.
func (a *AuthClient) Invalidate() {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.token = ""
	a.expires = time.Time{}
}

// ChallengeVerifySignedMessage assembles the exact bytes a `fauna.auth.verify`
// signature covers: the domain tag `fauna.auth.verify.v2\0`, then the 32-byte
// actor_id, then the 32-byte server-issued nonce, then the 32-byte identity of
// the nest the verify is addressed to (`login.md` § Binding the nest).
//
// This is the Go twin of Rust's single source,
// `fauna_protocol::auth::challenge_verify_signed_message` (tag
// `fauna_protocol::sig_domain::AUTH_VERIFY_V2`), which nest's `auth_core`
// verifier builds from. The two must move together: the actor key became
// **tagged-only** on 2026-08-17 — the untagged transition path was deleted in
// the same change, so a signer that omits the tag is refused outright with
// `fauna.auth.signature_failed`, and this Go signer was left behind by that
// sweep (it hand-rolls the ceremony rather than crossing the UniFFI boundary,
// so a Rust-side grep does not find it). The nest binding of 2026-09-23 moved
// both sides in one change. Kept next to the ceremony that uses it, exactly
// as enroll.go keeps EnrollmentSignedMessage — that sibling was already tagged
// (`fauna.bridges.enroll.v1`), which is why enrollment kept working while
// every bearer acquisition failed.
//
// Pure byte assembly, crypto-free, mirroring the Rust construction so the
// signer and verifier cannot drift.
func ChallengeVerifySignedMessage(actorID, nonce, nestID []byte) []byte {
	const domain = "fauna.auth.verify.v2\x00"
	msg := make([]byte, 0, len(domain)+len(actorID)+len(nonce)+len(nestID))
	msg = append(msg, domain...)
	msg = append(msg, actorID...)
	msg = append(msg, nonce...)
	msg = append(msg, nestID...)
	return msg
}

// CertBindingSignedMessage assembles the bytes the nest's deployment key signs
// in the tagged half of its channel binding (`auth_handlers::sign_channel_binding`,
// tag `fauna_protocol::sig_domain::CERT_BINDING_V1`): the served cert's SPKI
// (empty on a plaintext nest) followed by the client's nonce. The Go twin of
// `fauna_client_core::nest_trust::verify_binding_signature`'s tagged arm.
func CertBindingSignedMessage(spkiSha256, clientNonce []byte) []byte {
	const domain = "fauna.cert-binding.v1\x00"
	msg := make([]byte, 0, len(domain)+len(spkiSha256)+len(clientNonce))
	msg = append(msg, domain...)
	msg = append(msg, spkiSha256...)
	msg = append(msg, clientNonce...)
	return msg
}

// readLoginBinding learns the identity of the nest at the far end of c — the
// `nest_id` every login signature on this connection binds (`login.md`
// § Binding the nest; the Go twin of
// `fauna_client_core::nest_trust::read_login_binding`). The bridge asks the
// nest to sign a fresh client nonce (fauna.auth.nest_handshake) and verifies
// the tagged proof against the identity it claims: a box cannot claim an
// identity whose key it does not hold. Possession-only — the bridge does not
// compare the served cert's SPKI — the same residual the wasm arm documents.
// A nest that proves nothing, or whose proof fails, gets no login signature.
func readLoginBinding(ctx context.Context, c Caller) ([]byte, error) {
	var nonce [32]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		return nil, fmt.Errorf("wsrpc auth: login-binding nonce: %w", err)
	}
	var reply nestHandshakeReply
	if err := c.Call(ctx, methodAuthNestHandshake, nestHandshakeRequest{ClientNonce: nonce[:]}, &reply); err != nil {
		return nil, fmt.Errorf("wsrpc auth: fauna.auth.nest_handshake: %w", err)
	}
	if reply.CertBinding == nil {
		return nil, errors.New("wsrpc auth: the nest proved no identity to bind the login to")
	}
	nestID, err := hex.DecodeString(reply.CertBinding.NestActorID)
	if err != nil || len(nestID) != ed25519.PublicKeySize {
		return nil, fmt.Errorf("wsrpc auth: bad nest_actor_id hex (len=%d, err=%v)", len(nestID), err)
	}
	if len(reply.CertBinding.TaggedSig) != ed25519.SignatureSize {
		return nil, errors.New("wsrpc auth: the nest's identity binding carries no tagged signature")
	}
	msg := CertBindingSignedMessage(reply.CertBinding.SpkiSha256, nonce[:])
	if !ed25519.Verify(ed25519.PublicKey(nestID), msg, reply.CertBinding.TaggedSig) {
		return nil, errors.New("wsrpc auth: the nest's identity binding does not verify")
	}
	return nestID, nil
}

// acquireViaAnonymousWS opens a fresh anonymous (pre-identity) WS connection,
// runs the challenge/verify ceremony over it, and closes it — returning the
// fresh bearer token + its absolute expiry. The production acquireToken.
func (a *AuthClient) acquireViaAnonymousWS(ctx context.Context) (string, time.Time, error) {
	anon, err := DialAnonymous(ctx, AnonymousClientConfig{
		NestEndpoint: a.nestBaseURL,
		Logger:       a.logger,
		HTTPClient:   a.httpClient,
	})
	if err != nil {
		return "", time.Time{}, fmt.Errorf("wsrpc auth: dial anonymous ws: %w", err)
	}
	defer func() { _ = anon.Close() }()
	return a.acquireOverCaller(ctx, anon)
}

// acquireOverCaller runs the two-step challenge/verify ceremony over an
// already-open (anonymous) Caller and returns the bearer token + absolute
// expiry. Split from acquireViaAnonymousWS so the ceremony can be unit-tested
// with a fake Caller — no WS server.
func (a *AuthClient) acquireOverCaller(ctx context.Context, c Caller) (string, time.Time, error) {
	actorHex := hex.EncodeToString(a.actorID)

	// Step 0: the identity this login binds, read off THIS connection before
	// anything is signed (`login.md` § Binding the nest).
	nestID, err := readLoginBinding(ctx, c)
	if err != nil {
		return "", time.Time{}, err
	}

	// Step 1: fauna.auth.challenge → nonce.
	var chReply authChallengeReply
	if err := c.Call(ctx, methodAuthChallenge, authChallengeRequest{ActorID: actorHex}, &chReply); err != nil {
		return "", time.Time{}, fmt.Errorf("wsrpc auth: fauna.auth.challenge: %w", err)
	}
	nonce, err := hex.DecodeString(chReply.Nonce)
	if err != nil || len(nonce) != 32 {
		return "", time.Time{}, fmt.Errorf("wsrpc auth: bad nonce hex (len=%d, err=%v)", len(nonce), err)
	}

	// Step 2: sign the domain-tagged actor_id ‖ nonce ‖ nest_id.
	sig := ed25519.Sign(a.signKey, ChallengeVerifySignedMessage(a.actorID, nonce, nestID))

	// Step 3: fauna.auth.verify → bearer + absolute expiry.
	var vReply authVerifyReply
	if err := c.Call(ctx, methodAuthVerify, authVerifyRequest{
		ActorID:   actorHex,
		Nonce:     chReply.Nonce,
		Signature: hex.EncodeToString(sig),
		NestID:    hex.EncodeToString(nestID),
	}, &vReply); err != nil {
		return "", time.Time{}, fmt.Errorf("wsrpc auth: fauna.auth.verify: %w", err)
	}
	if vReply.Token == "" {
		return "", time.Time{}, errors.New("wsrpc auth: fauna.auth.verify returned empty token")
	}
	if vReply.ExpiresAt <= 0 {
		return "", time.Time{}, errors.New("wsrpc auth: fauna.auth.verify did not return expires_at")
	}
	return vReply.Token, time.Unix(vReply.ExpiresAt, 0), nil
}

// ActorIDHex returns the actor_id as the lowercase hex string the bridge
// uses in `/api/v1/ws/{actor_id_hex}`. Convenience helper for the dial
// path.
func (a *AuthClient) ActorIDHex() string {
	return hex.EncodeToString(a.actorID)
}
