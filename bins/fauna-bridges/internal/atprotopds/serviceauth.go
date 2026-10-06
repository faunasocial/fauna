package atprotopds

// Service auth — the short-lived, method-bound JWTs this PDS mints so a client
// (or the proxy path) can act as the account against another ATProto service
// (docs/goal/behavior/atproto-pds-full.md § F3 detail, *Service proxying*).
//
// The signature comes from the ACCOUNT's own repo signing key, never a
// bridge-wide one: per § Key material inventory that key signs repo commits
// *and* service JWTs, which is precisely what keeps C7's "no custody widening"
// true — service auth introduces no key class that did not already exist.
//
// Why the mint lives in Go rather than shared Rust, unlike D8 and the fetch
// guard: those two are *policy*, and policy is what must never fork. This is an
// encoding around a signer Go already owns. The key materializes on this side
// as an atcrypto private key for repo commits already (the unseal path in
// cmd/fauna-atproto-bridge), and indigo's atcrypto is the implementation the
// rest of the stack — PLC operations, repo commits — is already signed by.
// Re-implementing K-256 ECDSA in Rust for this one caller would add a second
// signer for the same key, on the same ecosystem's low-S/compact conventions,
// where a disagreement is silently rejected ecosystem-side rather than caught.
// Rust's k256 use today is key *generation* only, so there is nothing to share.
//
// This file stays cgo-free behind the RepoSignerSource seam (the Authorizer /
// safefetch.Guard pattern): production wires the per-user unseal from
// cmd/fauna-atproto-bridge, tests wire a fixed key.

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"net/http"
	"strconv"
	"strings"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// MaxServiceAuthLifetime is the ceiling on a minted token's validity
// (atproto-pds-full.md `:199`: `exp ≤ 60 s`). A hard-coded constant, never a
// knob — nobody would ever choose it (docs/goal/principles.md: the only
// configuration surface is the apps).
//
// Short by design: a service JWT is a bearer credential for one method against
// one audience, and it travels to a third party we do not control. Its lifetime
// is the whole revocation story, because nothing revokes it once issued.
const MaxServiceAuthLifetime = 60 * time.Second

// serviceAuthAlg is the JWS algorithm for a K-256 repo signing key. Pinned
// here, never read from a request: an attacker-chosen `alg` is the classic JWT
// confusion attack, and this minter has exactly one key type.
const serviceAuthAlg = "ES256K"

// RepoSigner is one account's repo signing key.
//
// A structural interface rather than atcrypto.PrivateKey (the concrete type,
// aliased as atprotorepo.Signer for the write path) so this package needs no
// indigo import and tests can drive the mint without one — the production key
// satisfies it as-is.
type RepoSigner interface {
	// HashAndSign returns the low-S, fixed-size r||s ECDSA-SHA256 signature
	// over content — the same form PlcOperation.Sign relies on.
	HashAndSign(content []byte) ([]byte, error)
}

// RepoSignerSource resolves the repo signing key for one account.
//
// A seam, not a map: production fetches the sealed per-user identity blob from
// nest and HPKE-opens it (cgo), and a future proxy path — which needs a key on
// *every* forwarded request rather than once per explicit mint — can put a
// cache behind this interface without the callers changing.
type RepoSignerSource interface {
	RepoSigner(ctx context.Context, actorID []byte) (RepoSigner, error)
}

// ServiceAuthRequest is one mint's claim set, before defaulting.
type ServiceAuthRequest struct {
	// Iss is the issuing account's DID. Always the authenticated caller's —
	// never a client-supplied value.
	Iss string
	// Aud is the target service DID (may carry a `#fragment`).
	Aud string
	// Lxm is the single method the token authorizes.
	Lxm string
	// Lifetime requested by the caller. Anything outside (0, MaxServiceAuth-
	// Lifetime] falls back to the maximum — see effectiveLifetime.
	Lifetime time.Duration
}

// effectiveLifetime applies the cap. A request outside the valid range is
// treated as "unspecified" rather than refused: every outcome is still bounded
// by the cap, so there is no sharp edge for a client to fall off, and no path
// where a caller's own arithmetic can widen the window.
func effectiveLifetime(requested time.Duration) time.Duration {
	if requested <= 0 || requested > MaxServiceAuthLifetime {
		return MaxServiceAuthLifetime
	}
	return requested
}

// serviceAuthHeader is the JWS header. A struct, not a map, so the field order
// on the wire is fixed by the type rather than by map iteration.
type serviceAuthHeader struct {
	Typ string `json:"typ"`
	Alg string `json:"alg"`
}

// serviceAuthClaims is the ratified claim set (`:199`) and nothing else. No
// `iat`: the goal doc pins iss/aud/lxm/jti/exp, and an unratified extra claim
// is a compatibility guess this slice has no way to verify (F5's live interop
// proof is where that would surface).
type serviceAuthClaims struct {
	Iss string `json:"iss"`
	Aud string `json:"aud"`
	Lxm string `json:"lxm"`
	Exp int64  `json:"exp"`
	Jti string `json:"jti"`
}

// MintServiceAuth builds and signs one service JWT.
//
// It refuses an incomplete claim set rather than emitting a token with an empty
// binding: a token with no `lxm` or no `aud` is a bearer credential for
// anything, anywhere, and that must be unrepresentable rather than merely
// unrequested.
func MintServiceAuth(signer RepoSigner, req ServiceAuthRequest, now time.Time) (string, error) {
	if signer == nil {
		return "", fmt.Errorf("service auth: no signing key")
	}
	if req.Iss == "" {
		return "", fmt.Errorf("service auth: no issuer DID")
	}
	if req.Aud == "" {
		return "", fmt.Errorf("service auth: no audience DID")
	}
	if req.Lxm == "" {
		return "", fmt.Errorf("service auth: no lxm — an unbound token is not mintable")
	}

	jti, err := newJTI()
	if err != nil {
		return "", err
	}

	hdr, err := json.Marshal(serviceAuthHeader{Typ: "JWT", Alg: serviceAuthAlg})
	if err != nil {
		return "", fmt.Errorf("service auth: marshal header: %w", err)
	}
	claims, err := json.Marshal(serviceAuthClaims{
		Iss: req.Iss,
		Aud: req.Aud,
		Lxm: req.Lxm,
		Exp: now.Add(effectiveLifetime(req.Lifetime)).Unix(),
		Jti: jti,
	})
	if err != nil {
		return "", fmt.Errorf("service auth: marshal claims: %w", err)
	}

	b64 := base64.RawURLEncoding.EncodeToString
	signingInput := b64(hdr) + "." + b64(claims)
	sig, err := signer.HashAndSign([]byte(signingInput))
	if err != nil {
		return "", fmt.Errorf("service auth: sign: %w", err)
	}
	return signingInput + "." + b64(sig), nil
}

// newJTI returns a fresh 128-bit token id. Random rather than a counter: the
// bridge is restartable and a counter would repeat across a restart, which is
// exactly when a replay-detecting service would start rejecting legitimate
// calls.
func newJTI() (string, error) {
	b := make([]byte, 16)
	if _, err := rand.Read(b); err != nil {
		return "", fmt.Errorf("service auth: generate jti: %w", err)
	}
	return base64.RawURLEncoding.EncodeToString(b), nil
}

// getServiceAuth implements com.atproto.server.getServiceAuth.
//
// The order below is load-bearing: validate, then authorize, then touch key
// material. A request that will be refused must never reach the unseal — key
// material is the most expensive and most sensitive thing this handler can
// touch, and a refused caller has no business causing either.
//
// NOTE on `iss`: it is the authenticated caller's DID — since slice 4d, the
// account's real DID, carried from the session token, so a minted token is
// resolvable by the external service it is presented to. (It was F1's
// `did:fauna:<hex>` placeholder until then, which made every minted token
// correctly signed but externally unresolvable.) F5's live interop proof is
// what exercises the real did:plc end of it.
func (s *Server) getServiceAuth(w http.ResponseWriter, r *http.Request, caller *xrpc.Caller) {
	q := r.URL.Query()
	aud := strings.TrimSpace(q.Get("aud"))
	lxm := strings.TrimSpace(q.Get("lxm"))
	if aud == "" || lxm == "" {
		xrpc.WriteError(w, xrpc.InvalidRequest("aud and lxm are both required"))
		return
	}

	// D8's SECOND check (atproto-pds-full.md `:193`). The frame's authz slot
	// already asked "may this credential mint at all"; this asks whether it may
	// mint for the method and audience actually requested. Skipping it would
	// silently bypass half of D8 — the route check alone never sees the
	// requested lxm, which is how migration-oriented minting stays
	// deferred-refused.
	if e := s.AuthorizeServiceAuth(caller, lxm, aud); e != nil {
		xrpc.WriteError(w, e)
		return
	}

	if s.signers == nil {
		s.logger.Error("getServiceAuth: no repo signer source wired")
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), rpcTimeout)
	defer cancel()
	signer, err := s.signers.RepoSigner(ctx, caller.ActorID)
	if err != nil {
		// Logged, not returned: the detail describes bridge-internal key state.
		s.logger.Warn("getServiceAuth: repo signing key unavailable", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}

	// One clock reading for both the requested-lifetime arithmetic and the
	// claim: two calls to time.Now() would measure the caller's window against
	// a different instant than the one the token is stamped with.
	now := time.Now()
	token, err := MintServiceAuth(signer, ServiceAuthRequest{
		Iss:      caller.DID,
		Aud:      aud,
		Lxm:      lxm,
		Lifetime: requestedLifetime(q.Get("exp"), now),
	}, now)
	if err != nil {
		s.logger.Warn("getServiceAuth: mint failed", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	xrpc.WriteJSON(w, map[string]any{"token": token})
}

// requestedLifetime turns the optional `exp` query parameter — an ABSOLUTE unix
// timestamp, the shape the lexicon uses — into a lifetime relative to now.
// Unparseable or absent yields 0, which effectiveLifetime reads as
// "unspecified" and answers with the cap.
func requestedLifetime(exp string, now time.Time) time.Duration {
	exp = strings.TrimSpace(exp)
	if exp == "" {
		return 0
	}
	secs, err := strconv.ParseInt(exp, 10, 64)
	if err != nil {
		return 0
	}
	return time.Unix(secs, 0).Sub(now)
}
