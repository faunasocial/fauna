package atprotopds

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"encoding/base64"
	"errors"
	"fmt"
	"math/big"
	"time"
)

// NestIssuerJWK is one public key from the nest's OAuth issuer key set, in the
// spelling the nest's own `/oauth/jwks` serves: a `kid` and the base64url
// P-256 coordinates the verifying key is rebuilt from.
//
// It is a type of this package rather than the WS-RPC package's so that
// rebuilding a key from coordinates happens beside the verifier that uses it,
// and so this package keeps no dependency on how the set arrived.
type NestIssuerJWK struct {
	Kid string
	X   string
	Y   string
}

// nestIssuerKeySet is what one atomic swap publishes: the nest's issuer
// identifier and the public keys it is currently serving.
//
// Both halves come from one reply for a reason. The PDS resource server refuses
// a nest-minted access token on two independent grounds — it pins `iss` and it
// resolves the signing key — and `authorization-server.md` § The issuer calls
// that "one gap with two halves, not two". Teaching one half and not the other
// leaves the token refused with the deployment believing otherwise, so the two
// facts arrive, and are replaced, together.
type nestIssuerKeySet struct {
	// issuer is the nest's issuer identifier, or "" while the nest has none to
	// name (a deployment with no claimed domain — § The issuer → *A nest with
	// no claimed domain has no issuer, and says so*). The nest decides this;
	// this side never derives it, and an empty issuer verifies nothing.
	issuer string
	// keys is the SERVED set, by `kid`. Absence is the whole meaning: the nest
	// applies the retirement horizon on the read this came from, so a key that
	// has left the set must stop verifying here at once. That is what bounds
	// the forced rotation arm's window at this verifier.
	keys map[string]*ecdsa.PublicKey
}

// nestIssuerMissRefetchInterval damps the unknown-`kid` refetch.
//
// A stranger who can present tokens at this resource server chooses their
// `kid`, so an undamped "refetch whenever a kid is unknown" would be a free
// amplifier pointed at the nest. One refetch per interval is enough for what
// the miss path is actually for (below), and the push is what carries urgency.
const nestIssuerMissRefetchInterval = 30 * time.Second

// SetNestIssuerKeys publishes a freshly fetched issuer key set, REPLACING
// whatever this side held.
//
// Replacing rather than merging is the point: a key the nest has stopped
// serving must stop verifying here, or the forced rotation arm — whose whole
// job is to make a leaked `kid` unusable on the next read — would be bounded by
// nothing at the one place it needs to reach
// (`authorization-server.md` § The issuer → *Two rotation arms*, "what the
// forced arm does not bound").
//
// A malformed coordinate fails the whole swap rather than dropping one key: a
// partially applied set is indistinguishable from a rotation, and would retire
// a key the issuer is still serving. Safe under live requests — readers load
// the set once per decision.
func (s *Server) SetNestIssuerKeys(issuer string, keys []NestIssuerJWK) error {
	set := &nestIssuerKeySet{issuer: issuer, keys: make(map[string]*ecdsa.PublicKey, len(keys))}
	for _, k := range keys {
		if k.Kid == "" {
			return errors.New("nest issuer key set: a key carries no kid")
		}
		pub, err := p256PublicFromJWKCoords(k.X, k.Y)
		if err != nil {
			return fmt.Errorf("nest issuer key set: kid %q: %w", k.Kid, err)
		}
		set.keys[k.Kid] = pub
	}
	s.nestIssuerKeys.Store(set)
	return nil
}

// NestIssuerURL is the issuer identifier this resource server currently
// honours, or "" if it honours none. Exported for the boot wiring's log line
// and for tests; the verify path reads the set directly.
func (s *Server) NestIssuerURL() string {
	set := s.nestIssuerKeys.Load()
	if set == nil {
		return ""
	}
	return set.issuer
}

// nestIssuerKeyForKID resolves a nest-minted access token's signing key.
//
// Returns no key while the nest names no issuer, so a deployment without one
// honours no OAuth token here — the resource server never verifies against an
// issuer it had to guess.
func (s *Server) nestIssuerKeyForKID(kid string) (*ecdsa.PublicKey, string) {
	set := s.nestIssuerKeys.Load()
	if set == nil || set.issuer == "" || kid == "" {
		return nil, ""
	}
	pub := set.keys[kid]
	if pub == nil {
		return nil, ""
	}
	return pub, set.issuer
}

// noteUnknownIssuerKID asks the refresh loop for a re-read, at most once per
// [nestIssuerMissRefetchInterval].
//
// ⚠ What this path is for is easy to get backwards. On a FORCED rotation the
// dropped `kid` is one this side still holds, so a token signed by it hits no
// miss at all — the push is what removes it, and the ticker is the backstop if
// the push is lost. The miss is the opposite case: an HONEST client arriving
// with a token signed by a key minted after our last read. So this path is
// availability, the push is the compromise response, and both are named in the
// pickup rule (`authorization-server.md` § The issuer → *Two rotation arms*).
func (s *Server) noteUnknownIssuerKID() {
	if s.issuerKeyNudge == nil {
		return
	}
	now := s.oauthClock()
	last := s.lastIssuerMissRefetch.Load()
	if last != 0 && now.Sub(time.Unix(0, last)) < nestIssuerMissRefetchInterval {
		return
	}
	if !s.lastIssuerMissRefetch.CompareAndSwap(last, now.UnixNano()) {
		// Another request won the race and is already asking; one nudge per
		// interval is the whole contract.
		return
	}
	select {
	case s.issuerKeyNudge <- struct{}{}:
	default:
	}
}

// SetIssuerKeyNudge attaches the channel the refresh loop waits on. Buffered by
// the caller and written non-blockingly, like every other nudge in this bridge:
// a lost nudge costs a ticker period, never a request.
func (s *Server) SetIssuerKeyNudge(ch chan<- struct{}) { s.issuerKeyNudge = ch }

// p256PublicFromJWKCoords rebuilds a P-256 public key from the base64url
// coordinates a JWKS carries.
//
// The width check is not decoration: `elliptic.Unmarshal`'s replacement takes
// big.Ints, which silently accept a short encoding, and a coordinate that lost
// a leading zero byte would produce a different point rather than an error.
// The on-curve check is what rejects a point that is not a P-256 point at all.
func p256PublicFromJWKCoords(x, y string) (*ecdsa.PublicKey, error) {
	xb, err := base64.RawURLEncoding.DecodeString(x)
	if err != nil {
		return nil, fmt.Errorf("x is not base64url: %w", err)
	}
	yb, err := base64.RawURLEncoding.DecodeString(y)
	if err != nil {
		return nil, fmt.Errorf("y is not base64url: %w", err)
	}
	if len(xb) != p256CoordBytes || len(yb) != p256CoordBytes {
		return nil, fmt.Errorf("coordinates are %d/%d bytes, want %d each",
			len(xb), len(yb), p256CoordBytes)
	}
	pub := &ecdsa.PublicKey{
		Curve: elliptic.P256(),
		X:     new(big.Int).SetBytes(xb),
		Y:     new(big.Int).SetBytes(yb),
	}
	if !pub.Curve.IsOnCurve(pub.X, pub.Y) {
		return nil, errors.New("coordinates are not a point on P-256")
	}
	return pub, nil
}
