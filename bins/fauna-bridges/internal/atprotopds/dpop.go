package atprotopds

// F4 slice 4 — the resource server's half of DPoP (RFC 9449): the three things
// a pure policy module cannot do, and nothing else.
//
//	1. Verify an ES256 signature, and derive the key's `jkt` thumbprint.
//	2. Issue and recognise server-issued nonces, which needs a secret and a
//	   clock.
//	3. Remember which proofs have been seen, which needs a bounded store.
//
// Everything a proof *means* — which `typ` and `alg` are acceptable, what the
// embedded key must look like, which claims must be present, what `htm`/`htu`
// must equal, how old an `iat` may be — lives in shared Rust
// (`fauna_bridge_atproto::dpop`) and reaches here over the seam, exactly like
// D8. The compact JWT is decomposed there and only there:
// this file verifies over the `SigningInput` the module hands back, and never
// re-splits the token, because two splitters judging one token is the classic
// JWT parser differential.
//
// The package stays cgo-free: production wires the FFI adapter from
// cmd/fauna-atproto-bridge, tests wire a fake.

import (
	"crypto/ecdh"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"log/slog"
	"math/big"
	"net/http"
	"sync"
	"time"
)

// ── Seam types, mirroring the shared-Rust ones ───────────────────────────────

// DPoPExpectations mirrors `fauna_bridge_atproto::dpop::DpopExpectations` —
// the facts about this request that only the serving process knows.
type DPoPExpectations struct {
	HTM         string
	HTU         string
	NowUnix     int64
	MaxAgeSecs  uint32
	MaxSkewSecs uint32
	// ExpectedAth selects the shared module's `ath` rule: nil at an
	// authorization-server endpoint, non-nil at a resource-server request,
	// where `ath` is REQUIRED and must equal this value:
	// `base64url(sha256(access token))` of the token the request actually
	// presented (RFC 9449 §4.3). This process is only ever the resource
	// server, so [Server.dpopGate] always passes one.
	ExpectedAth *string
}

// DPoPProof mirrors `fauna_bridge_atproto::dpop::DpopProof`: a proof that
// passed every policy check, decomposed so this side can verify it without
// re-parsing the token.
type DPoPProof struct {
	SigningInput string
	Signature    []byte
	PublicKeyX   []byte
	PublicKeyY   []byte
	JTI          string
	Nonce        string
	IssuedAt     int64
}

// DPoPVerdict mirrors `fauna_bridge_atproto::dpop::DpopVerdict`. Exactly one
// field is set.
type DPoPVerdict struct {
	Proof *DPoPProof
	Deny  *OAuthDeny
}

// ── Bounds ───────────────────────────────────────────────────────────────────

const (
	// dpopNonceWindow — the granularity of a nonce. A nonce names the window
	// it was minted in, and is accepted while that window or the next one is
	// current, so the widest a nonce's life ever gets is two windows.
	dpopNonceWindow = 2 * time.Minute
	// dpopNonceAcceptedWindows — the current window plus its predecessor. One
	// window alone would expire a nonce the instant the clock ticked over,
	// mid-flight, for a client that did nothing wrong.
	dpopNonceAcceptedWindows = 2
	// dpopProofMaxAge is therefore both the nonce's maximum life and the
	// `iat` window a proof is accepted in — deliberately ONE constant. The
	// spec's ceiling is five minutes (§ Ecosystem reality item 3); four
	// leaves headroom, so widening the accepted-window count later cannot
	// silently cross the ceiling.
	dpopProofMaxAge = dpopNonceWindow * dpopNonceAcceptedWindows
	// dpopProofMaxSkew — how far ahead of us a client's clock may be. A proof
	// is minted milliseconds before it is sent, so anything beyond a small
	// allowance is a clock that is simply wrong, not latency.
	dpopProofMaxSkew = 30 * time.Second

	// dpopNonceSecretRotation — how often the bridge-memory nonce secret is
	// replaced (§ Key material inventory, the rotating *DPoP nonce secret*
	// row). Freshness is bounded by the window index inside the MAC, not by
	// this: rotation limits how long one secret is used, and the
	// one-generation overlap below only prevents a seam at the rotation
	// instant.
	dpopNonceSecretRotation = time.Hour
	// dpopNonceBytes — how much of the MAC becomes the nonce. A nonce is an
	// unforgeability token, not a key: 144 bits is far past any forgery
	// budget, and the shorter string keeps the header small.
	dpopNonceBytes = 18
	// dpopNonceDomain separates this MAC from any other use of the same
	// secret, the same discipline the sealed-blob AADs use.
	dpopNonceDomain = "fauna-atproto-dpop-nonce-v1\x00"

	// dpopReplayCapacity bounds the resource-server plane's seen-`jti` set.
	//
	// ⚠ **Derived against the budget that actually governs this caller**: an XRPC request spends
	// its ROUTE's class, and the three classes that reach an authenticated
	// route are ClassAuthed (120/60s), ClassWrite (30/60s) and ClassBlob
	// (20/60s) — 170 per minute, so **680 entries per source IP per 4-minute
	// window**. 65536 is ≈96 peers at the per-IP ceiling, or ≈550 at the
	// 30-requests-per-minute an ordinary polling client actually spends.
	// Memory is bounded because the KEY is bounded — a `jkt` is 43 base64url
	// chars and the shared module caps `jti` at 256
	// (`fauna_bridge_atproto::dpop::JTI_MAX_LEN`, capped for exactly this
	// reason) — so the ceiling is ~24 MB worst case and ~10 MB at realistic
	// identifier lengths.
	//
	// ⚠ **This set CAN be driven to evicting a live entry by a plausible
	// population** rather than only by an implausible one. That is not a
	// defect to size away — a 1024-peer margin would cost ~700k entries — it
	// is why the eviction warning below is load-bearing rather than merely
	// tidy, and why it names its set.
	dpopReplayCapacity = 65536

	// dpopNonceHeader is where a client reads the nonce it must echo.
	dpopNonceHeader = "DPoP-Nonce"
	// dpopProofHeader carries the proof itself.
	dpopProofHeader = "DPoP"
)

// ── The nonce ────────────────────────────────────────────────────────────────

// dpopNonceMinter issues and recognises server nonces.
//
// A nonce is a MAC over the window it belongs to — `HMAC(secret, domain ||
// window)` — and NOT a random token recorded in a set. A MAC has no set to
// overflow, and freshness comes from the window index inside it, so a nonce
// cannot be made to live longer by any amount of traffic.
//
// ⚠ **That statelessness is load-bearing, and it is what licenses the frame's
// ordering**. The XRPC frame
// issues a nonce at step 0 — for any request presenting the DPoP scheme, ahead
// of routing AND ahead of the rate limiter, which is deliberate (a client must
// be able to recover from a 429 or a 404). That is an anonymous, unmetered call
// into Mint on the internet-facing mux, and it is safe for exactly one reason:
// minting allocates nothing. **Replacing this with a random-token-in-a-set
// minter would therefore not be a local change** — it would silently turn step
// 0 into an anonymous memory-growth vector, which the frame's ordering comment
// would not catch. Anything stored per nonce needs the rate limiter moved above
// step 0 first, and that trade re-opens the closed issue.
type dpopNonceMinter struct {
	mu sync.Mutex
	// current signs every nonce issued from now on; previous is kept for one
	// rotation so a nonce minted a moment before rotation still verifies.
	current   [32]byte
	previous  [32]byte
	hasPrev   bool
	rotatedAt time.Time
	now       func() time.Time
}

func newDPoPNonceMinter(now func() time.Time) *dpopNonceMinter {
	if now == nil {
		now = time.Now
	}
	m := &dpopNonceMinter{now: now}
	m.mu.Lock()
	m.rotateLocked(now())
	m.mu.Unlock()
	return m
}

// rotateLocked replaces the secret when its period is up.
//
// The fresh secret is generated into a temporary first and installed only on
// success: a failed read must leave the minter serving under the secret it
// already has, never under a zero one.
func (m *dpopNonceMinter) rotateLocked(now time.Time) {
	if !m.rotatedAt.IsZero() && now.Sub(m.rotatedAt) < dpopNonceSecretRotation {
		return
	}
	var fresh [32]byte
	if _, err := rand.Read(fresh[:]); err != nil {
		return
	}
	if !m.rotatedAt.IsZero() {
		m.previous, m.hasPrev = m.current, true
	}
	m.current = fresh
	m.rotatedAt = now
}

// Mint issues a nonce for the current window.
func (m *dpopNonceMinter) Mint() string {
	m.mu.Lock()
	defer m.mu.Unlock()
	now := m.now()
	m.rotateLocked(now)
	return nonceFor(m.current, windowIndex(now))
}

// Accepts reports whether a nonce is one this server issued recently.
//
// It re-derives rather than looks up, over the (at most two) secrets and the
// (at most two) live windows. Comparison is constant-time throughout: a nonce
// is an unforgeability token, and a byte-at-a-time comparison would leak the
// prefix an attacker had got right.
func (m *dpopNonceMinter) Accepts(nonce string) bool {
	if nonce == "" {
		return false
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	now := m.now()
	m.rotateLocked(now)
	current := windowIndex(now)
	secrets := [][32]byte{m.current}
	if m.hasPrev {
		secrets = append(secrets, m.previous)
	}
	// Deliberately not short-circuiting on the first match: the loop is a
	// fixed four comparisons at most, and its cost is the same either way.
	found := false
	for _, secret := range secrets {
		for w := current; w > current-dpopNonceAcceptedWindows; w-- {
			if hmac.Equal([]byte(nonce), []byte(nonceFor(secret, w))) {
				found = true
			}
		}
	}
	return found
}

// windowIndex names the window a nonce belongs to. Freshness is derived from
// this index alone, never from when a secret rotated — which is what bounds a
// nonce's life at two windows no matter what else happens.
//
// ⚠ One clock behaviour worth naming (offered as
// a nicety rather than an ask): a **backwards wall-clock step** re-opens nonces
// and proofs that had expired, because the index walks back with the clock.
// What blunts it is the `jti` replay set — entries persist past their expiry
// until capacity pressure evicts them, so they re-compare against the restored
// clock — and the residual is bounded by `ath`, `htm` and `htu`, which pin a
// replayed proof to the one request it was captured on. Do not "fix" this with a
// monotonic clock without re-deciding what a nonce means across a process
// restart.
func windowIndex(now time.Time) int64 {
	return now.UnixNano() / int64(dpopNonceWindow)
}

func nonceFor(secret [32]byte, window int64) string {
	mac := hmac.New(sha256.New, secret[:])
	mac.Write([]byte(dpopNonceDomain))
	var buf [8]byte
	binary.BigEndian.PutUint64(buf[:], uint64(window))
	mac.Write(buf[:])
	return base64.RawURLEncoding.EncodeToString(mac.Sum(nil)[:dpopNonceBytes])
}

// ── The replay set ───────────────────────────────────────────────────────────

// replaySet remembers a single-use identifier until it can no longer be used.
//
// Small by construction rather than by policy: a proof must carry a live
// nonce, so no proof is acceptable for longer than one nonce's life, so nothing
// needs remembering for longer than that either.
//
// ⚠ **Overflow evicts the entry nearest to expiry, and that direction was
// decided for THIS plane**. A resource-server
// proof accompanies a 15-minute MULTI-use access token, so an evicted `jti`
// re-opens a replay whose companion is not spent. But that changes how much an
// eviction COSTS, not which way to fail: what the replay buys is a duplicate of
// one already-captured request (`htm`, `htu` and `ath` bind the proof to exactly
// that one) by an attacker who already holds the captured pair, i.e. who already
// has TLS compromise. Failing closed instead refuses **every call of every live
// session** for as long as the set is full, answered as the uniform
// `AuthenticationRequired` a client cannot act on — the plane-wide outage had just been shipped and fixed. Eviction under pressure is logged
// with the set's name, because a degraded defence should be visible rather than
// silent.
//
// Recording an entry here requires an access token the issuer signed
// (`verifyOAuthAccessToken` runs before the gate), so the set can only be
// pressed by holders of real grants — never by an anonymous flood.
type replaySet struct {
	mu       sync.Mutex
	seen     map[string]time.Time
	now      func() time.Time
	logger   *slog.Logger
	name     string
	capacity int
}

func newReplaySet(name string, capacity int, now func() time.Time, logger *slog.Logger) *replaySet {
	if now == nil {
		now = time.Now
	}
	if logger == nil {
		logger = slog.Default()
	}
	return &replaySet{
		seen:     map[string]time.Time{},
		now:      now,
		logger:   logger,
		name:     name,
		capacity: capacity,
	}
}

// Record notes an identifier within its `scope` and reports whether that pair
// had already been seen.
//
// ⚠ **`scope` is a parameter, not a convention, and that is the whole point**. Every identifier this
// set tracks is *caller-chosen* — a `jti` is an arbitrary string the client puts
// in its own proof — and RFC 9449 does not make one globally unique. Keyed on the
// bare string, any caller could therefore spend an identifier an honest caller
// was about to present, and the honest request would be refused as a replay — a
// targeted availability denial. Taking the scope in the signature is what makes
// that unrepresentable rather than remembered: there is no way to call this with
// a bare identifier.
//
// The scope is the proving key's `jkt` thumbprint — the identity the proof's
// `jti` is scoped to.
//
// `expires` is when the entry may be forgotten — for a DPoP proof, the window
// this server mints nonces in.
//
// Test-and-insert under one lock, so two concurrent replays of the same
// identifier cannot both find the set empty.
func (s *replaySet) Record(scope, id string, expires time.Time) (replayed bool) {
	key := replayKey(scope, id)
	s.mu.Lock()
	defer s.mu.Unlock()
	now := s.now()
	if seen, ok := s.seen[key]; ok && now.Before(seen) {
		return true
	}
	if len(s.seen) >= s.capacity {
		s.evictLocked(now)
	}
	s.seen[key] = expires
	return false
}

// replayKey joins a scope and a caller-chosen identifier into one map key.
//
// The encoding is injective because **every scope this set is given is NUL-free
// by construction** — a `jkt` is base64url — so the first NUL always
// delimits, and no `(scope, id)` pair can spell the same key as a different one. The identifier half needs no such
// guarantee, which is the right way round: it is the half an attacker chooses.
func replayKey(scope, id string) string {
	return scope + "\x00" + id
}

func (s *replaySet) evictLocked(now time.Time) {
	freed := false
	for k, expires := range s.seen {
		if !now.Before(expires) {
			delete(s.seen, k)
			freed = true
		}
	}
	if freed {
		return
	}
	var oldestKey string
	var oldest time.Time
	for k, expires := range s.seen {
		if oldestKey == "" || expires.Before(oldest) {
			oldestKey, oldest = k, expires
		}
	}
	if oldestKey != "" {
		delete(s.seen, oldestKey)
		// Every entry was still live, so this dropped an identifier that could
		// now be replayed. Seeing it means the set is under more pressure than
		// dpopReplayCapacity was sized for.
		s.logger.Warn("atproto oauth: replay set evicted a live entry — replay window open",
			"set", s.name, "capacity", s.capacity)
	}
}

// ── Verification ─────────────────────────────────────────────────────────────

// verifyDPoPSignature checks an ES256 signature over the module's signing input
// and returns the proving key.
//
// The point is validated before use (crypto/ecdh's constructor rejects a point
// that is not on P-256 or is the identity). A bad point could only ever fail
// verification, so this is not load-bearing for the signature — it is
// load-bearing for the *thumbprint*: `jkt` is computed from the coordinates,
// and a nonsense point must not be allowed to become a binding a later token
// carries.
func verifyDPoPSignature(proof *DPoPProof) (*ecdsa.PublicKey, bool) {
	const coordBytes = 32
	if len(proof.PublicKeyX) != coordBytes || len(proof.PublicKeyY) != coordBytes {
		return nil, false
	}
	if len(proof.Signature) != 2*coordBytes {
		return nil, false
	}
	uncompressed := make([]byte, 0, 1+2*coordBytes)
	uncompressed = append(uncompressed, 4)
	uncompressed = append(uncompressed, proof.PublicKeyX...)
	uncompressed = append(uncompressed, proof.PublicKeyY...)
	if _, err := ecdh.P256().NewPublicKey(uncompressed); err != nil {
		return nil, false
	}
	pub := &ecdsa.PublicKey{
		Curve: elliptic.P256(),
		X:     new(big.Int).SetBytes(proof.PublicKeyX),
		Y:     new(big.Int).SetBytes(proof.PublicKeyY),
	}
	digest := sha256.Sum256([]byte(proof.SigningInput))
	r := new(big.Int).SetBytes(proof.Signature[:coordBytes])
	s := new(big.Int).SetBytes(proof.Signature[coordBytes:])
	if !ecdsa.Verify(pub, digest[:], r, s) {
		return nil, false
	}
	return pub, true
}

// ── The gate ─────────────────────────────────────────────────────────────────

// dpopGate runs a resource-server request's DPoP proof through policy, crypto,
// nonce and replay checks, and returns the thumbprint of the key it proved
// possession of.
//
// `htu` is the URL this request was made to, built from this PDS's own
// published origin (see [oauthTokenVerifier.VerifyAccess]), and `ath` is the
// hash of the access token presented beside the proof — the proof must carry
// exactly that (RFC 9449 §4.3).
//
// The ORDER is the contract, and each step is where it is for a reason:
//
//  1. **Exactly one proof header.** Two `DPoP` headers is a request that means
//     two different things to two different readers.
//  2. **Policy** (shared Rust). Everything decidable from the token alone,
//     including that a nonce is present at all — a first-contact proof gets the
//     retryable `use_dpop_nonce` from here.
//  3. **Nonce**, before the signature. It is the cheap check, it is the one a
//     correct client fails when its held nonce ages out, and answering it early
//     costs an attacker nothing: nonces are public and this server hands one
//     out with every response anyway.
//  4. **Signature.** Nothing before this point has a side effect, so nothing
//     unverified has been written down.
//  5. **Replay**, strictly AFTER the signature. Recording a `jti` from an
//     unverified proof would let anyone poison the set with an honest client's
//     future identifiers and lock it out — the check would become the attack.
func (s *Server) dpopGate(r *http.Request, htu, ath string) (jkt string, deny *OAuthDeny) {
	headers := r.Header.Values(dpopProofHeader)
	if len(headers) != 1 {
		description := "a DPoP proof is required on this endpoint"
		if len(headers) > 1 {
			description = "more than one DPoP header was sent"
		}
		return "", &OAuthDeny{Error: oauthErrInvalidDPoP, Description: description}
	}

	verdict := s.dpopPolicy.ValidateDPoPProof(headers[0], DPoPExpectations{
		HTM:         r.Method,
		HTU:         htu,
		ExpectedAth: &ath,
		NowUnix:     s.oauthClock().Unix(),
		// One window for both: a proof is exactly as fresh as the nonce it
		// must carry, so there is no second freshness rule to keep in step.
		MaxAgeSecs:  uint32(dpopProofMaxAge / time.Second),
		MaxSkewSecs: uint32(dpopProofMaxSkew / time.Second),
	})
	if verdict.Proof == nil {
		if verdict.Deny != nil {
			return "", verdict.Deny
		}
		return "", &OAuthDeny{
			Error:       oauthErrServerErr,
			Description: "DPoP policy returned no verdict",
		}
	}

	if !s.dpopNonces.Accepts(verdict.Proof.Nonce) {
		return "", &OAuthDeny{
			Error:       oauthErrUseDPoPNonce,
			Description: "the DPoP nonce is not one this server issued recently — retry with the one in this response's DPoP-Nonce header",
		}
	}

	pub, ok := verifyDPoPSignature(verdict.Proof)
	if !ok {
		return "", &OAuthDeny{
			Error:       oauthErrInvalidDPoP,
			Description: "the DPoP proof's signature does not verify under the key it carries",
		}
	}

	// The RFC-7638 thumbprint, the same function the nest's authorization
	// server derives `cnf.jkt` with — so the binding a token carries and the key
	// a proof presents are compared in one spelling.
	//
	// It is derived BEFORE the replay record because it is also the record's
	// scope. This does not disturb the ordering that IS a security
	// property — the record still happens strictly after `verifyDPoPSignature`
	// above, which is what stops a forged proof burning an honest caller's
	// identifier (`TestAFailedProofDoesNotBurnItsIdentifier`). Deriving a
	// thumbprint is a pure function of the already-validated point; it records
	// nothing.
	thumb, err := ecThumbprint(pub)
	if err != nil {
		s.logger.Error("atproto oauth: DPoP thumbprint failed", "err", err)
		return "", &OAuthDeny{
			Error:       oauthErrServerErr,
			Description: "could not derive the DPoP key thumbprint",
		}
	}

	if s.dpopReplays.Record(thumb, verdict.Proof.JTI, s.oauthClock().Add(dpopProofMaxAge)) {
		return "", &OAuthDeny{
			Error:       oauthErrInvalidDPoP,
			Description: "this DPoP proof has already been used",
		}
	}
	return thumb, nil
}

// ── The thumbprint ───────────────────────────────────────────────────────────

// jwk is the public JSON Web Key shape the thumbprint is computed over. Field
// order matters: RFC 7638 hashes the canonical JSON of exactly {crv, kty, x, y}
// in lexicographic order, and Go's encoding/json emits struct fields in
// declaration order, so declaring them sorted makes marshalling this struct BE
// the thumbprint input.
type jwk struct {
	Crv string `json:"crv"`
	Kty string `json:"kty"`
	X   string `json:"x"`
	Y   string `json:"y"`
}

// publicJWK renders the thumbprint-canonical members for a P-256 public key.
//
// Coordinates are fixed-width 32-byte big-endian, zero-padded — NOT the
// variable-length big.Int encoding. A coordinate that happens to have a leading
// zero byte would otherwise serialize one byte short, producing a thumbprint
// that silently differs from every other implementation's for the same key —
// and a `cnf.jkt` that never matches.
func publicJWK(pub *ecdsa.PublicKey) jwk {
	return jwk{
		Crv: "P-256",
		Kty: "EC",
		X:   b64Coord(pub.X),
		Y:   b64Coord(pub.Y),
	}
}

func b64Coord(v *big.Int) string {
	buf := make([]byte, p256CoordBytes)
	v.FillBytes(buf)
	return base64.RawURLEncoding.EncodeToString(buf)
}

// ecThumbprint is the RFC-7638 JWK thumbprint: base64url(sha256(canonical JWK)).
func ecThumbprint(pub *ecdsa.PublicKey) (string, error) {
	canonical, err := json.Marshal(publicJWK(pub))
	if err != nil {
		return "", fmt.Errorf("canonical JWK: %w", err)
	}
	sum := sha256.Sum256(canonical)
	return base64.RawURLEncoding.EncodeToString(sum[:]), nil
}
