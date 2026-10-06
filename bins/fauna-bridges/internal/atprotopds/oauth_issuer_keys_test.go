package atprotopds

import (
	"encoding/base64"
	"testing"
	"time"
)

// A nest-minted access token is refused until this resource server has been
// taught the nest's issuer — even when it already holds the signing key.
//
// The issuer is the half the nest withholds while it has none to name (a
// domainless deployment): keys alone must never be enough, or a token could
// verify with no `iss` pinned at all.
func TestANestMintedTokenIsRefusedUntilTheIssuerIsHonoured(t *testing.T) {
	s, _, _ := rsServer(t)
	nestKey := newTestIssuerKey(t)
	token := mintNestAccessToken(t, s, nestKey, nestIssuerURL, clientSigner)

	// Keys fed, issuer withheld.
	if err := s.SetNestIssuerKeys("", []NestIssuerJWK{nestKey.feed()}); err != nil {
		t.Fatalf("SetNestIssuerKeys: %v", err)
	}
	if _, err := s.verifyOAuthAccessToken(token); err == nil {
		t.Fatal("a nest-minted token verified while the deployment names no issuer")
	}

	// The nest answers with its issuer.
	if err := s.SetNestIssuerKeys(nestIssuerURL, []NestIssuerJWK{nestKey.feed()}); err != nil {
		t.Fatalf("SetNestIssuerKeys: %v", err)
	}
	claims, err := s.verifyOAuthAccessToken(token)
	if err != nil {
		t.Fatalf("a nest-minted token is still refused once the issuer is honoured: %v", err)
	}
	if claims.Iss != nestIssuerURL {
		t.Errorf("iss = %q, want %q", claims.Iss, nestIssuerURL)
	}
}

// `iss` is pinned to the nest's fed issuer. A token signed by a served key but
// naming any other issuer — the retired bridge AS's own origin included — is
// refused, so a key being in the set never licenses a token to claim to come
// from somewhere else.
func TestATokenMustNameTheFedIssuer(t *testing.T) {
	s, _, _ := rsServer(t)
	nestKey := honourNestIssuer(t, s)

	for _, iss := range []string{testPDSOrigin, "https://elsewhere.example", ""} {
		if _, err := s.verifyOAuthAccessToken(
			mintNestAccessToken(t, s, nestKey, iss, clientSigner)); err == nil {
			t.Errorf("a token signed by the nest's key was accepted claiming iss %q", iss)
		}
	}
	// The same key under the fed issuer still verifies, so the refusals above
	// are the pin and not a broken verify path.
	if _, err := s.verifyOAuthAccessToken(
		mintNestAccessToken(t, s, nestKey, nestIssuerURL, clientSigner)); err != nil {
		t.Errorf("the nest's own token no longer verifies: %v", err)
	}
}

// A key the nest has stopped serving stops verifying here on the next read.
//
// This is what bounds the forced rotation arm's window at a verifier: the arm
// drops every key that existed, and if this side merged sets instead of
// replacing them, the leaked `kid` would keep verifying here forever — at the
// one place the arm exists to reach (§ The issuer → Two rotation arms, "what
// the forced arm does not bound").
func TestAKeyTheNestStoppedServingStopsVerifyingHere(t *testing.T) {
	s, _, _ := rsServer(t)
	leaked := honourNestIssuer(t, s)
	token := mintNestAccessToken(t, s, leaked, nestIssuerURL, clientSigner)
	if _, err := s.verifyOAuthAccessToken(token); err != nil {
		t.Fatalf("the served key must verify before the rotation: %v", err)
	}

	// The forced arm: a new signer, and every other key dropped from the set.
	fresh := newTestIssuerKey(t)
	if err := s.SetNestIssuerKeys(nestIssuerURL, []NestIssuerJWK{fresh.feed()}); err != nil {
		t.Fatalf("SetNestIssuerKeys: %v", err)
	}
	if _, err := s.verifyOAuthAccessToken(token); err == nil {
		t.Fatal("a token signed by a force-rotated key still verifies — this side merged " +
			"the sets instead of replacing")
	}
	if _, err := s.verifyOAuthAccessToken(
		mintNestAccessToken(t, s, fresh, nestIssuerURL, clientSigner)); err != nil {
		t.Errorf("the replacement key does not verify: %v", err)
	}
}

// A malformed coordinate refuses the whole swap rather than dropping one key.
//
// A partially applied set is indistinguishable from a rotation, so it would
// silently retire a key the issuer is still serving — an outage that looks like
// correct behaviour.
func TestAMalformedKeyRefusesTheWholeSwap(t *testing.T) {
	s, _, _ := rsServer(t)
	good := honourNestIssuer(t, s)
	token := mintNestAccessToken(t, s, good, nestIssuerURL, clientSigner)

	for _, tc := range []struct {
		name string
		key  NestIssuerJWK
	}{
		{"no kid", NestIssuerJWK{Kid: "", X: good.feed().X, Y: good.feed().Y}},
		{"not base64url", NestIssuerJWK{Kid: "k", X: "!!!", Y: good.feed().Y}},
		// A coordinate that lost a leading zero byte is a DIFFERENT point, not
		// an error, once it reaches big.Int — so the width check is what makes
		// this a refusal rather than a silently wrong key.
		{"short coordinate", NestIssuerJWK{
			Kid: "k",
			X:   base64.RawURLEncoding.EncodeToString(make([]byte, p256CoordBytes-1)),
			Y:   good.feed().Y,
		}},
		{"off the curve", NestIssuerJWK{
			Kid: "k",
			X:   base64.RawURLEncoding.EncodeToString(make([]byte, p256CoordBytes)),
			Y:   base64.RawURLEncoding.EncodeToString(make([]byte, p256CoordBytes)),
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if err := s.SetNestIssuerKeys(nestIssuerURL, []NestIssuerJWK{tc.key}); err == nil {
				t.Fatal("the swap was accepted")
			}
			if _, err := s.verifyOAuthAccessToken(token); err != nil {
				t.Errorf("the previously held set was disturbed by a refused swap: %v", err)
			}
		})
	}
}

// An unknown `kid` asks for one re-read per interval, not one per request.
//
// A stranger presenting tokens at this resource server chooses the `kid`, so an
// undamped miss path is a free amplifier pointed at the nest.
func TestAnUnknownKidAsksForOneRereadPerInterval(t *testing.T) {
	s, _, clock := rsServer(t)
	nudge := make(chan struct{}, 8)
	s.SetIssuerKeyNudge(nudge)
	if err := s.SetNestIssuerKeys(nestIssuerURL, nil); err != nil {
		t.Fatalf("SetNestIssuerKeys: %v", err)
	}

	stranger := newTestIssuerKey(t)
	for range 5 {
		// Refused: its kid is not in the served set. That is the miss.
		if _, err := s.verifyOAuthAccessToken(
			mintNestAccessToken(t, s, stranger, nestIssuerURL, clientSigner)); err == nil {
			t.Fatal("a token signed by an unserved key verified")
		}
	}
	if got := len(nudge); got != 1 {
		t.Fatalf("%d re-reads asked for by 5 misses in one interval, want 1", got)
	}

	clock.advance(nestIssuerMissRefetchInterval + time.Second)
	if _, err := s.verifyOAuthAccessToken(
		mintNestAccessToken(t, s, stranger, nestIssuerURL, clientSigner)); err == nil {
		t.Fatal("a token signed by an unserved key verified")
	}
	if got := len(nudge); got != 2 {
		t.Fatalf("%d re-reads after the interval elapsed, want 2 — an honest client "+
			"arriving with a token from a key minted since our last read would wait "+
			"for the ticker", got)
	}
}
