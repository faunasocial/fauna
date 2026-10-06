package atprotopds

// The PDS as an OAuth RESOURCE SERVER — the half of F4 that stays on this
// bridge (`authorization-server.md` § The issuer).
//
// The authorization server is the nest's, on the apex domain: it runs PAR, the
// consent ceremony, the token and revocation endpoints, and it holds the ES256
// issuer key. This bridge only ever sees the artifact that ceremony produces —
// a DPoP-bound access token presented on an XRPC call — and verifies it
// against the nest's served key set (oauth_issuer_keys.go), pinning `iss` to
// the nest's issuer and requiring this PDS's service DID among the token's
// `aud`, then checks the DPoP proof riding beside it (dpop.go).
//
// The bridge's own authorization server — PAR, the consent page, /oauth/token,
// /oauth/revoke, its ES256 key set and JWKS — retired in the same change that
// re-pointed the protected-resource document at the nest (§ The issuer → *The
// re-point, the teaching, and the bridge AS's retirement are ONE change*).
//
// The package stays cgo-free: production wires the FFI DPoP policy from
// cmd/fauna-atproto-bridge, tests wire a fake.

import (
	"crypto/ecdsa"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"math/big"
	"net/http"
	"strings"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// ── Seam types, mirroring the shared-Rust ones ───────────────────────────────

// OAuthDeny mirrors the `{error, description}` pair a refusal from
// `fauna_bridge_atproto::dpop` carries. The error is an RFC 9449 code; Go never
// interprets it beyond the one retryable case the frame answers by name
// ([oauthErrUseDPoPNonce]).
type OAuthDeny struct {
	Error       string
	Description string
}

// DPoPPolicy is the seam to `fauna_bridge_atproto::dpop` — everything a proof
// MEANS. Same discipline as Authorizer: Go carries data across and enforces the
// verdict, and a nil seam refuses rather than improvising a decision.
type DPoPPolicy interface {
	ValidateDPoPProof(compact string, expect DPoPExpectations) DPoPVerdict
}

// The RFC 9449 codes, plus the one this side answers when its own seam is out
// of step. At a resource server every refusal flattens to the uniform
// `AuthenticationRequired` except `use_dpop_nonce`, which survives as the
// frame's retryable challenge (see [oauthTokenVerifier.VerifyAccess]).
const (
	oauthErrServerErr    = "server_error"
	oauthErrInvalidDPoP  = "invalid_dpop_proof"
	oauthErrUseDPoPNonce = "use_dpop_nonce"
)

const (
	// PlaneOAuth is the session plane a nest-minted OAuth grant belongs to —
	// what D8 receives as the caller's plane, so the matrix judges an OAuth
	// grant as one rather than as an app password.
	PlaneOAuth = "oauth"

	// oauthAccessTokenTyp is the JOSE `typ` of an OAuth access token (RFC
	// 9068). Pinning it is what lets this resource server refuse a token
	// minted for another purpose under the same key rather than inspecting
	// claims to find out.
	oauthAccessTokenTyp = "at+jwt"

	// p256CoordBytes is the fixed width of a P-256 scalar in the JOSE
	// encodings — both the ES256 signature halves and the JWKS coordinates.
	p256CoordBytes = 32
)

// oauthAccessClaims is the claim set of a nest-minted ES256 access token, as
// this resource server reads it.
//
// `Cnf.Jkt` is the binding the whole plane exists to carry: the thumbprint of
// the key the client proved possession of at the nest's PAR, re-checked here
// against the DPoP proof accompanying every call. A token without it is a
// bearer token wearing a DPoP costume.
type oauthAccessClaims struct {
	Iss   string        `json:"iss"`
	Sub   string        `json:"sub"`
	Aud   oauthAudience `json:"aud"`
	Scope string        `json:"scope"`
	Iat   int64         `json:"iat"`
	Exp   int64         `json:"exp"`
	Jti   string        `json:"jti"`
	Cnf   oauthCnf      `json:"cnf"`
	Sid   string        `json:"sid,omitempty"`
	CliID string        `json:"client_id,omitempty"`
	Actor string        `json:"fauna_actor,omitempty"`
}

// oauthAudience is an access token's `aud`: the resource servers it may be
// presented at (`authorization-server.md` § The issuer → *The audience is the
// set of readers*).
//
// The nest spells one reader as a JSON string and two as an array (RFC 7519
// §4.1.3 allows both), so both are read. This server only ever asks whether
// its own identifier is a member ([oauthAudience.names]); the other members
// belong to the other readers.
type oauthAudience []string

// UnmarshalJSON reads either wire spelling. Anything else — a number, an array
// with a non-string member — is an error, which refuses the token.
func (a *oauthAudience) UnmarshalJSON(raw []byte) error {
	var one string
	if err := json.Unmarshal(raw, &one); err == nil {
		*a = oauthAudience{one}
		return nil
	}
	var many []string
	if err := json.Unmarshal(raw, &many); err != nil {
		return err
	}
	*a = many
	return nil
}

// MarshalJSON writes the nest's spelling: one reader a string, otherwise an
// array.
func (a oauthAudience) MarshalJSON() ([]byte, error) {
	if len(a) == 1 {
		return json.Marshal(a[0])
	}
	return json.Marshal([]string(a))
}

// names reports whether `reader` is a member of the audience.
func (a oauthAudience) names(reader string) bool {
	for _, member := range a {
		if member == reader {
			return true
		}
	}
	return false
}

// oauthCnf is RFC 9449's confirmation claim.
type oauthCnf struct {
	Jkt string `json:"jkt"`
}

// oauthJoseHeader is the JOSE header of an ES256 access token. `Kid` selects
// the verifying key from the nest's served set — a lookup, never "the current
// key", which is what keeps a rotation from breaking tokens in flight.
type oauthJoseHeader struct {
	Alg string `json:"alg"`
	Typ string `json:"typ"`
	Kid string `json:"kid"`
}

// EnableOAuthResourceServer wires the OAuth plane's verifier: the DPoP policy
// seam, and this PDS's own origin (`https://pds.<apex>`) — the prefix every
// resource-server proof's `htu` is built from.
//
// `pdsOrigin` must come from the shared-Rust origin builder
// (`oauth_metadata::oauth_issuer` over the PDS host), never from a request's
// Host header: `htu` is compared for equality, so taking it from the request
// would let a caller reaching this process under any other name satisfy the
// check against a URL we never published.
//
// Without this call the OAuth plane refuses every token — the state of a
// domainless bridge, which has no issuer to honour anyway.
func (s *Server) EnableOAuthResourceServer(policy DPoPPolicy, pdsOrigin string) {
	s.dpopPolicy = policy
	s.pdsOrigin = pdsOrigin
	s.dpopNonces = newDPoPNonceMinter(s.oauthClock)
	s.dpopReplays = newReplaySet("dpop-proof-rs", dpopReplayCapacity, s.oauthClock, s.logger)
}

// verifyOAuthAccessToken verifies an ES256 access token minted by the nest's
// authorization server.
//
// The signing key is resolved by `kid` over the nest's served set
// ([Server.nestIssuerKeyForKID]), and `iss` is pinned to the issuer that set
// was fed with. No issuer fed — a domainless deployment, or a bridge that has
// not completed its first key read — resolves no key, so every token is
// refused rather than verified against a guess.
func (s *Server) verifyOAuthAccessToken(token string) (*oauthAccessClaims, error) {
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return nil, errInvalidToken
	}
	rawHeader, err := base64.RawURLEncoding.DecodeString(parts[0])
	if err != nil {
		return nil, errInvalidToken
	}
	var header oauthJoseHeader
	if err := json.Unmarshal(rawHeader, &header); err != nil {
		return nil, errInvalidToken
	}
	// `alg` is pinned rather than read: an attacker-chosen algorithm is the
	// oldest JWT break there is, and `typ` is pinned so a token minted for
	// another purpose under this key cannot be replayed as an access token.
	if header.Alg != "ES256" || header.Typ != oauthAccessTokenTyp {
		return nil, errInvalidToken
	}
	pub, wantIssuer := s.nestIssuerKeyForKID(header.Kid)
	if pub == nil {
		s.noteUnknownIssuerKID()
		return nil, errInvalidToken
	}
	sig, err := base64.RawURLEncoding.DecodeString(parts[2])
	if err != nil || len(sig) != 2*p256CoordBytes {
		return nil, errInvalidToken
	}
	digest := sha256.Sum256([]byte(parts[0] + "." + parts[1]))
	r := new(big.Int).SetBytes(sig[:p256CoordBytes])
	sv := new(big.Int).SetBytes(sig[p256CoordBytes:])
	if !ecdsa.Verify(pub, digest[:], r, sv) {
		return nil, errInvalidToken
	}
	rawClaims, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return nil, errInvalidToken
	}
	var claims oauthAccessClaims
	if err := json.Unmarshal(rawClaims, &claims); err != nil {
		return nil, errInvalidToken
	}
	now := s.oauthClock().Unix()
	if claims.Exp <= now || claims.Iat > now+60 {
		return nil, errInvalidToken
	}
	// `iss` so a token minted by a different authorization server cannot be
	// presented here, `aud` so one the nest minted for a different service
	// cannot either. The nest names this PDS's service DID among the token's
	// audiences when the grant holds an ATProto-family scope
	// (`oauth_metadata::pds_service_did`, the same owner the minter's
	// serviceDID is read through at boot); a token addressed to the nest alone
	// — an OIDC-only sign-in — is not for this server.
	if claims.Iss != wantIssuer || !claims.Aud.names(s.minter.serviceDID) {
		return nil, errInvalidToken
	}
	if claims.Cnf.Jkt == "" {
		return nil, errInvalidToken
	}
	return &claims, nil
}

// oauthTokenVerifier adapts the server to xrpc.TokenVerifier for the OAuth
// plane. A distinct type rather than a second method on *Server, so the two
// planes' verifiers cannot be confused at a wiring site — the frame takes them
// as two arguments and swapping them would otherwise compile.
type oauthTokenVerifier struct{ s *Server }

// OAuthTokenVerifier is the xrpc.TokenVerifier for OAuth access tokens. Safe to
// construct on any server; one that was never given a resource-server wiring
// or an issuer refuses every token.
func (s *Server) OAuthTokenVerifier() xrpc.TokenVerifier { return oauthTokenVerifier{s: s} }

// IssueNonce implements xrpc.NonceIssuer: a fresh nonce on **every** response,
// so the plane that requires one is also the plane that supplies one.
//
// ⚠ **This is not an optimisation; without it the plane is broken.** A nonce
// lives `dpopProofMaxAge` (4 min) and a nest-minted access token 15 minutes, so
// a client whose only source of nonces were an authorization-server round trip
// would be refused for the last eleven minutes of every token — and recover
// only by spending a refresh rotation, which restores service just long enough
// to look like flakiness rather than a defect. The nest's endpoints issue their own nonces under their own
// secret; a DPoP nonce is per-server by design (RFC 9449 §8), so this plane
// cannot borrow them.
//
// Returns "" before [Server.EnableOAuthResourceServer] has run. The frame sets
// no header on "" rather than an empty one, which is what keeps an unwired
// process from advertising a nonce it cannot recognise.
func (v oauthTokenVerifier) IssueNonce() string {
	if v.s == nil || v.s.dpopNonces == nil {
		return ""
	}
	return v.s.dpopNonces.Mint()
}

// VerifyAccess verifies an OAuth access token AND the DPoP proof that must
// accompany it (RFC 9449 §7).
//
// Three checks make the binding real, and dropping any one of them silently
// turns these tokens back into bearer credentials:
//
//   - the proof is over THIS request (`htm`/`htu`), so one captured on another
//     endpoint cannot be presented here;
//   - its `ath` is the hash of the token actually presented, so a proof
//     captured alongside one token cannot be replayed alongside another;
//   - the proving key's thumbprint equals the token's own `cnf.jkt`, which is
//     what ties the credential to the key its ceremony bound it to.
//
// The `htu` is built from this server's own PDS origin and the request path,
// never from `r.Host`: a caller reaching this process under another name must
// not be able to satisfy an equality check against a URL we never published.
func (v oauthTokenVerifier) VerifyAccess(r *http.Request, token string) (*xrpc.Caller, error) {
	s := v.s
	if s.pdsOrigin == "" || s.dpopPolicy == nil {
		return nil, errInvalidToken
	}
	claims, err := s.verifyOAuthAccessToken(token)
	if err != nil {
		return nil, err
	}
	// The hash is computed HERE, from the token this request presented, and
	// handed to the policy module — which owns the comparison but none of the
	// crypto, the same split every other F4 seam uses.
	sum := sha256.Sum256([]byte(token))
	ath := base64.RawURLEncoding.EncodeToString(sum[:])
	jkt, deny := s.dpopGate(r, s.pdsOrigin+r.URL.Path, ath)
	if deny != nil {
		// The gate's ONE retryable refusal survives this boundary by name, so
		// the frame can answer `WWW-Authenticate: DPoP error="use_dpop_nonce"`
		// and the client retries with the nonce riding the same response. Every
		// other deny — a bad signature, a wrong `ath`, a replayed `jti` —
		// flattens to the uniform refusal below, unchanged.
		//
		// ⚠ Reachable only BELOW `verifyOAuthAccessToken`, which is what makes
		// the challenge safe to distinguish: a caller that receives one has
		// already presented a token the issuer signed, so the answer is never
		// an oracle about the token (see xrpc.ErrUseDPoPNonce).
		if deny.Error == oauthErrUseDPoPNonce {
			return nil, xrpc.ErrUseDPoPNonce
		}
		return nil, errInvalidToken
	}
	if jkt != claims.Cnf.Jkt {
		return nil, errInvalidToken
	}
	actor, err := hex.DecodeString(claims.Actor)
	if err != nil || len(actor) != 32 {
		return nil, errInvalidToken
	}
	sid, err := base64.RawURLEncoding.DecodeString(claims.Sid)
	if err != nil || len(sid) == 0 {
		return nil, errInvalidToken
	}
	return &xrpc.Caller{
		ActorID:   actor,
		DID:       claims.Sub,
		Scope:     claims.Scope,
		SessionID: sid,
		Plane:     PlaneOAuth,
	}, nil
}
