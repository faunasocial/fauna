// Package atprotopds implements the F1 auth core of the ATProto PDS bridge
// (docs/goal/behavior/atproto-pds-full.md § F1 detail): the HS256 session
// token model and the com.atproto.server.* XRPC handlers.
//
// Token model: access JWT — HS256 under a bridge-scoped secret (verified
// only by this bridge; no cross-service key distribution), claims sub =
// user DID, aud = the PDS service DID, scope = com.atproto.appPass (or the
// DM-privileged variant), 60 min. Refresh JWT — jti rotates on use with
// reuse detection (the nest registry holds the current jti; a replay kills
// the session family), sid = the immutable family id, 90 days.
//
// The JWT encoding is deliberately hand-rolled on stdlib crypto: the bridge
// mints and verifies ONLY its own tokens (XRPC clients treat them as opaque
// strings), so a third-party JWT library would add attack surface for no
// interop gain. Verification pins alg to HS256 — nothing else, never
// "none" — and compares MACs constant-time.
package atprotopds

import (
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"
)

// Hard-coded token lifetimes (F1 detail; ecosystem-aligned).
const (
	AccessTokenLifetime  = 60 * time.Minute
	RefreshTokenLifetime = 90 * 24 * time.Hour
)

// Scopes (ecosystem constants, re-verified at convergence).
const (
	ScopeAppPass           = "com.atproto.appPass"
	ScopeAppPassPrivileged = "com.atproto.appPassPrivileged"
	ScopeRefresh           = "com.atproto.refresh"
)

// PlaneAppCredential is the F1 session plane recorded in the nest registry.
const PlaneAppCredential = "app_credential"

// SecretSource yields the bridge-scoped HS256 secret. The production
// source unseals the nest-stored blob sealed to the bridge's attested
// x25519 (the mail TLS blob pattern — § Key material inventory);
// tests inject a fixed secret.
type SecretSource interface {
	SessionTokenSecret() ([]byte, error)
}

// StaticSecret is a SecretSource for tests and the pre-sealing boot path.
type StaticSecret []byte

func (s StaticSecret) SessionTokenSecret() ([]byte, error) { return []byte(s), nil }

// TokenMinter mints and verifies the bridge's own session JWTs.
type TokenMinter struct {
	secrets SecretSource
	// serviceDID is the PDS service DID used as aud.
	serviceDID string
	clock      func() time.Time
}

// NewTokenMinter builds a minter. A nil clock uses the real clock.
func NewTokenMinter(secrets SecretSource, serviceDID string, clock func() time.Time) *TokenMinter {
	if clock == nil {
		clock = time.Now
	}
	return &TokenMinter{secrets: secrets, serviceDID: serviceDID, clock: clock}
}

// Claims is the JWT claim set this bridge mints (subset of RFC 7519 +
// ATProto's sid convention on refresh tokens).
type Claims struct {
	Sub   string `json:"sub"`
	Aud   string `json:"aud"`
	Scope string `json:"scope"`
	Iat   int64  `json:"iat"`
	Exp   int64  `json:"exp"`
	Jti   string `json:"jti"`
	// Sid is the session-family id: base64url of the initial refresh jti
	// bytes (both token kinds carry it — access tokens so getSession can
	// name its session, refresh tokens for the rotation check).
	Sid string `json:"sid,omitempty"`
	// Handle the session was created with (echoed by getSession /
	// refreshSession replies without a nest round-trip).
	Handle string `json:"handle,omitempty"`
	// Ascope (refresh tokens only) records the ORIGINAL access-token scope
	// so a rotation re-mints the same app-pass variant (plain vs
	// DM-privileged) without re-consulting the minting credential.
	Ascope string `json:"ascope,omitempty"`
	// FaunaActor is the hex 32-byte Fauna actor id this session authenticates
	// — the binding every nest-facing call needs (record/refresh/end session,
	// ingest_external_write, the D8 input).
	//
	// It has its own claim because `sub` is the account's REAL DID (slice 4d).
	// Before 4d the bridge derived the actor back out of `sub` by hex-decoding
	// a `did:fauna:<hex>` placeholder, which is what put the write path and the
	// projection loop in two different repos. The binding is no less
	// trustworthy here: this token is minted AND verified by this bridge under
	// the sealed HS256 secret, so a claim in it is exactly as authentic as
	// `sub` was — the actor id was already a claim, merely smuggled inside the
	// DID string. Carrying it explicitly also keeps the auth path free of a
	// DID→actor lookup, which would need a cache that refuses valid tokens for
	// any account minted since its last refresh.
	//
	// Not secret: the actor id is the account's public identity key, and it was
	// already fully exposed in the old `sub`. It is now strictly less prominent.
	FaunaActor string `json:"fauna_actor,omitempty"`
	// Plane names which credential plane this token belongs to (F4 slice 7).
	// Every minter stamps it — this bridge mints only [PlaneAppCredential] —
	// and a token without it is refused: absence names no plane, and an
	// unnamed plane is never guessed (the compat-remnant sweep retired the
	// empty-means-app reading, docs/goal/architecture/version-compatibility.md
	// § Dimension 2).
	//
	// ⚠ The claim is what keeps `com.atproto.server.refreshSession` an
	// app-plane verb: a refresh token of any other plane rotated there would
	// mint an app-plane ACCESS token, which is not DPoP-bound, from a
	// credential that was. [TokenMinter.VerifyRefreshScope] and
	// [TokenMinter.VerifyAccessScope] both require the app plane by name.
	Plane string `json:"plane,omitempty"`
}

// ActorBytes decodes the fauna_actor claim. A token without a well-formed
// 32-byte actor is refused rather than defaulted: the actor id decides which
// account every nest-facing call acts on, so guessing it is unthinkable, and a
// token without the claim (malformed or forged) must fail closed into a re-login.
func (c *Claims) ActorBytes() ([]byte, error) {
	if c.FaunaActor == "" {
		return nil, errInvalidToken
	}
	b, err := hex.DecodeString(c.FaunaActor)
	if err != nil || len(b) != 32 {
		return nil, errInvalidToken
	}
	return b, nil
}

// AccessScope is the access-token scope a rotation should re-mint.
func (c *Claims) AccessScope() string {
	if c.Ascope == "" {
		return ScopeAppPass
	}
	return c.Ascope
}

// NewSessionTokens is the minted pair for one session.
type NewSessionTokens struct {
	AccessJwt  string
	RefreshJwt string
	// SessionID is the family id (initial refresh jti), as registered
	// nest-side via record_session.
	SessionID []byte
	// RefreshJti is the current refresh jti bytes (== SessionID at mint).
	RefreshJti []byte
	// ExpiresAt is the refresh expiry, epoch millis (the nest convention).
	ExpiresAt int64
}

// randomJti mints 16 random bytes — the jti/session-id granularity.
func randomJti() ([]byte, error) {
	b := make([]byte, 16)
	if _, err := rand.Read(b); err != nil {
		return nil, fmt.Errorf("mint jti: %w", err)
	}
	return b, nil
}

func b64(b []byte) string  { return base64.RawURLEncoding.EncodeToString(b) }
func jsonB64(v any) string { j, _ := json.Marshal(v); return b64(j) }

// MintSession mints the access+refresh pair for a fresh session. did is
// the account's real ATProto DID (sub), actorID the 32-byte Fauna actor it
// belongs to; handle rides as a claim for getSession echoes; scope picks the
// app-pass variant by dm_allowed.
func (m *TokenMinter) MintSession(did string, actorID []byte, handle, scope string) (*NewSessionTokens, error) {
	sessionID, err := randomJti()
	if err != nil {
		return nil, err
	}
	now := m.clock()
	refreshExp := now.Add(RefreshTokenLifetime)
	access, err := m.mintAccess(did, actorID, handle, scope, sessionID, now)
	if err != nil {
		return nil, err
	}
	refresh, err := m.sign(Claims{
		Sub:        did,
		Aud:        m.serviceDID,
		Scope:      ScopeRefresh,
		Iat:        now.Unix(),
		Exp:        refreshExp.Unix(),
		Jti:        b64(sessionID),
		Sid:        b64(sessionID),
		Handle:     handle,
		Ascope:     scope,
		FaunaActor: hex.EncodeToString(actorID),
		Plane:      PlaneAppCredential,
	})
	if err != nil {
		return nil, err
	}
	return &NewSessionTokens{
		AccessJwt:  access,
		RefreshJwt: refresh,
		SessionID:  sessionID,
		RefreshJti: sessionID,
		ExpiresAt:  refreshExp.UnixMilli(),
	}, nil
}

// RotatedTokens is the pair minted by a refresh rotation.
type RotatedTokens struct {
	AccessJwt  string
	RefreshJwt string
	NewJti     []byte
	ExpiresAt  int64
}

// MintRotation mints the replacement pair for a rotate-on-use refresh:
// same session family (sid), fresh jti, fresh lifetimes, original scope.
func (m *TokenMinter) MintRotation(did string, actorID []byte, handle, scope string, sessionID []byte) (*RotatedTokens, error) {
	newJti, err := randomJti()
	if err != nil {
		return nil, err
	}
	now := m.clock()
	refreshExp := now.Add(RefreshTokenLifetime)
	access, err := m.mintAccess(did, actorID, handle, scope, sessionID, now)
	if err != nil {
		return nil, err
	}
	refresh, err := m.sign(Claims{
		Sub:        did,
		Aud:        m.serviceDID,
		Scope:      ScopeRefresh,
		Iat:        now.Unix(),
		Exp:        refreshExp.Unix(),
		Jti:        b64(newJti),
		Sid:        b64(sessionID),
		Handle:     handle,
		Ascope:     scope,
		FaunaActor: hex.EncodeToString(actorID),
		Plane:      PlaneAppCredential,
	})
	if err != nil {
		return nil, err
	}
	return &RotatedTokens{
		AccessJwt:  access,
		RefreshJwt: refresh,
		NewJti:     newJti,
		ExpiresAt:  refreshExp.UnixMilli(),
	}, nil
}

func (m *TokenMinter) mintAccess(did string, actorID []byte, handle, scope string, sessionID []byte, now time.Time) (string, error) {
	jti, err := randomJti()
	if err != nil {
		return "", err
	}
	return m.sign(Claims{
		Sub:        did,
		Aud:        m.serviceDID,
		Scope:      scope,
		Iat:        now.Unix(),
		Exp:        now.Add(AccessTokenLifetime).Unix(),
		Jti:        b64(jti),
		Sid:        b64(sessionID),
		Handle:     handle,
		FaunaActor: hex.EncodeToString(actorID),
		Plane:      PlaneAppCredential,
	})
}

var jwtHeader = jsonB64(map[string]string{"alg": "HS256", "typ": "JWT"})

func (m *TokenMinter) sign(c Claims) (string, error) {
	secret, err := m.secrets.SessionTokenSecret()
	if err != nil {
		return "", fmt.Errorf("session token secret: %w", err)
	}
	signing := jwtHeader + "." + jsonB64(c)
	mac := hmac.New(sha256.New, secret)
	mac.Write([]byte(signing))
	return signing + "." + b64(mac.Sum(nil)), nil
}

// Verification errors are deliberately coarse — the XRPC frame maps every
// failure to one uniform AuthenticationRequired anyway.
var errInvalidToken = errors.New("invalid token")

// Verify parses + verifies a token minted by this bridge: HS256 only
// (pinned header), constant-time MAC compare, exp/iat sanity, aud match.
func (m *TokenMinter) Verify(token string) (*Claims, error) {
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return nil, errInvalidToken
	}
	// Pin the exact header this bridge mints — rejects alg confusion
	// ("none", RS256, …) by construction.
	if parts[0] != jwtHeader {
		return nil, errInvalidToken
	}
	secret, err := m.secrets.SessionTokenSecret()
	if err != nil {
		return nil, fmt.Errorf("session token secret: %w", err)
	}
	mac := hmac.New(sha256.New, secret)
	mac.Write([]byte(parts[0] + "." + parts[1]))
	want, err := base64.RawURLEncoding.DecodeString(parts[2])
	if err != nil || !hmac.Equal(mac.Sum(nil), want) {
		return nil, errInvalidToken
	}
	payload, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return nil, errInvalidToken
	}
	var c Claims
	if err := json.Unmarshal(payload, &c); err != nil {
		return nil, errInvalidToken
	}
	now := m.clock()
	if c.Exp <= now.Unix() || c.Iat > now.Unix()+60 {
		return nil, errInvalidToken
	}
	if c.Aud != m.serviceDID {
		return nil, errInvalidToken
	}
	return &c, nil
}

// VerifyAccessScope verifies a token and requires an app-pass scope (a
// refresh token presented as an access token must fail) and the app plane by
// name (see [Claims.Plane]).
func (m *TokenMinter) VerifyAccessScope(token string) (*Claims, error) {
	c, err := m.Verify(token)
	if err != nil {
		return nil, err
	}
	if c.Scope != ScopeAppPass && c.Scope != ScopeAppPassPrivileged {
		return nil, errInvalidToken
	}
	if c.Plane != PlaneAppCredential {
		return nil, errInvalidToken
	}
	return c, nil
}

// VerifyRefreshScope verifies a token and requires the refresh scope with a
// session id (an access token presented at refreshSession must fail).
//
// It also requires the APP plane by name: `com.atproto.server.refreshSession`
// is the app-credential plane's own lifecycle verb (the plane boundary § F4
// detail's *Scope model* settles), so a refresh token naming any other plane —
// or none — is refused here. Rotating one through this path would mint an
// app-plane access token — a bearer credential — from whatever it was, which is
// the laundering the [Claims.Plane] doc names.
func (m *TokenMinter) VerifyRefreshScope(token string) (*Claims, error) {
	c, err := m.Verify(token)
	if err != nil {
		return nil, err
	}
	if c.Scope != ScopeRefresh || c.Sid == "" {
		return nil, errInvalidToken
	}
	if c.Plane != PlaneAppCredential {
		return nil, errInvalidToken
	}
	return c, nil
}

// SidBytes decodes the session-family id claim.
func (c *Claims) SidBytes() ([]byte, error) {
	b, err := base64.RawURLEncoding.DecodeString(c.Sid)
	if err != nil || len(b) == 0 {
		return nil, errInvalidToken
	}
	return b, nil
}

// JtiBytes decodes the jti claim.
func (c *Claims) JtiBytes() ([]byte, error) {
	b, err := base64.RawURLEncoding.DecodeString(c.Jti)
	if err != nil || len(b) == 0 {
		return nil, errInvalidToken
	}
	return b, nil
}
