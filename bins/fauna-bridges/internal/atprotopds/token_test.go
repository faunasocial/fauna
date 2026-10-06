package atprotopds

import (
	"strings"
	"testing"
	"time"
)

const testServiceDID = "did:web:pds.example.com"

func testMinter(t *testing.T, clock func() time.Time) *TokenMinter {
	t.Helper()
	return NewTokenMinter(StaticSecret("0123456789abcdef0123456789abcdef"), testServiceDID, clock)
}

func TestMintVerifyRoundTrip(t *testing.T) {
	m := testMinter(t, nil)
	tokens, err := m.MintSession(testLoginDID, testActorID, "alice", ScopeAppPass)
	if err != nil {
		t.Fatal(err)
	}

	access, err := m.VerifyAccessScope(tokens.AccessJwt)
	if err != nil {
		t.Fatalf("access verify: %v", err)
	}
	if access.Sub != testLoginDID || access.Handle != "alice" || access.Scope != ScopeAppPass {
		t.Fatalf("access claims: %+v", access)
	}
	sid, err := access.SidBytes()
	if err != nil || len(sid) != 16 {
		t.Fatalf("access sid: %v %d", err, len(sid))
	}

	refresh, err := m.VerifyRefreshScope(tokens.RefreshJwt)
	if err != nil {
		t.Fatalf("refresh verify: %v", err)
	}
	if refresh.AccessScope() != ScopeAppPass {
		t.Fatalf("refresh ascope: %q", refresh.Ascope)
	}
	rsid, _ := refresh.SidBytes()
	rjti, _ := refresh.JtiBytes()
	if string(rsid) != string(tokens.SessionID) || string(rjti) != string(tokens.RefreshJti) {
		t.Fatal("refresh sid/jti mismatch with minted session")
	}
}

func TestScopeConfusionRefused(t *testing.T) {
	m := testMinter(t, nil)
	tokens, err := m.MintSession(testLoginDID, testActorID, "alice", ScopeAppPassPrivileged)
	if err != nil {
		t.Fatal(err)
	}
	// A refresh token presented as an access token must fail, and vice versa.
	if _, err := m.VerifyAccessScope(tokens.RefreshJwt); err == nil {
		t.Fatal("refresh token accepted as access token")
	}
	if _, err := m.VerifyRefreshScope(tokens.AccessJwt); err == nil {
		t.Fatal("access token accepted as refresh token")
	}
}

func TestVerifyRejectsTampering(t *testing.T) {
	m := testMinter(t, nil)
	tokens, err := m.MintSession(testLoginDID, testActorID, "alice", ScopeAppPass)
	if err != nil {
		t.Fatal(err)
	}
	good := tokens.AccessJwt
	parts := strings.Split(good, ".")

	cases := map[string]string{
		"empty":            "",
		"two segments":     parts[0] + "." + parts[1],
		"tampered payload": parts[0] + "." + parts[1][:len(parts[1])-2] + "xx" + "." + parts[2],
		"tampered mac":     parts[0] + "." + parts[1] + "." + parts[2][:len(parts[2])-2] + "xx",
		// alg-none / alg-swap: any header other than the pinned HS256 one.
		"alg none":  "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0." + parts[1] + ".",
		"alg RS256": "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9." + parts[1] + "." + parts[2],
	}
	for name, bad := range cases {
		if _, err := m.Verify(bad); err == nil {
			t.Errorf("%s: accepted", name)
		}
	}

	// Wrong secret refuses.
	m2 := NewTokenMinter(StaticSecret("another-secret-another-secret-xx"), testServiceDID, nil)
	if _, err := m2.Verify(good); err == nil {
		t.Fatal("token verified under a different secret")
	}
	// Wrong audience refuses.
	m3 := NewTokenMinter(StaticSecret("0123456789abcdef0123456789abcdef"), "did:web:other.example", nil)
	if _, err := m3.Verify(good); err == nil {
		t.Fatal("token verified under a different service DID")
	}
}

func TestExpiryEnforced(t *testing.T) {
	now := time.Unix(1_760_000_000, 0)
	clock := func() time.Time { return now }
	m := testMinter(t, clock)
	tokens, err := m.MintSession(testLoginDID, testActorID, "alice", ScopeAppPass)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := m.Verify(tokens.AccessJwt); err != nil {
		t.Fatalf("fresh access token refused: %v", err)
	}
	now = now.Add(AccessTokenLifetime + time.Second)
	if _, err := m.Verify(tokens.AccessJwt); err == nil {
		t.Fatal("expired access token accepted")
	}
	// The refresh token (90 d) still verifies…
	if _, err := m.VerifyRefreshScope(tokens.RefreshJwt); err != nil {
		t.Fatalf("refresh refused at 60min: %v", err)
	}
	// …until it too expires.
	now = now.Add(RefreshTokenLifetime)
	if _, err := m.VerifyRefreshScope(tokens.RefreshJwt); err == nil {
		t.Fatal("expired refresh token accepted")
	}
}

func TestRotationKeepsFamilyAndScope(t *testing.T) {
	m := testMinter(t, nil)
	tokens, err := m.MintSession(testLoginDID, testActorID, "alice", ScopeAppPassPrivileged)
	if err != nil {
		t.Fatal(err)
	}
	rc, err := m.VerifyRefreshScope(tokens.RefreshJwt)
	if err != nil {
		t.Fatal(err)
	}
	sid, _ := rc.SidBytes()
	rotated, err := m.MintRotation(rc.Sub, testActorID, rc.Handle, rc.AccessScope(), sid)
	if err != nil {
		t.Fatal(err)
	}
	ac, err := m.VerifyAccessScope(rotated.AccessJwt)
	if err != nil {
		t.Fatal(err)
	}
	if ac.Scope != ScopeAppPassPrivileged {
		t.Fatalf("rotation lost the privileged scope: %q", ac.Scope)
	}
	rc2, err := m.VerifyRefreshScope(rotated.RefreshJwt)
	if err != nil {
		t.Fatal(err)
	}
	sid2, _ := rc2.SidBytes()
	jti2, _ := rc2.JtiBytes()
	if string(sid2) != string(sid) {
		t.Fatal("rotation changed the session family id")
	}
	if string(jti2) == string(tokens.RefreshJti) {
		t.Fatal("rotation did not change the jti")
	}
	if string(jti2) != string(rotated.NewJti) {
		t.Fatal("rotated refresh jti claim != NewJti")
	}
}

// refreshSession is the app plane's own verb: a refresh token naming another
// plane is refused by its plane claim, because rotating it here would mint an
// unbound app-plane access token from a DPoP-bound credential (the laundering
// [Claims.Plane] names).
func TestRefreshSessionRefusesAnOAuthPlaneRefreshToken(t *testing.T) {
	m := testMinter(t, nil)
	now := time.Now()
	oauth, err := m.sign(Claims{
		Sub:        testLoginDID,
		Aud:        testServiceDID,
		Scope:      ScopeRefresh,
		Iat:        now.Unix(),
		Exp:        now.Add(time.Hour).Unix(),
		Jti:        b64([]byte("oauth-jti")),
		Sid:        b64([]byte("oauth-sid")),
		FaunaActor: strings.Repeat("ab", 32),
		Plane:      PlaneOAuth,
	})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := m.VerifyRefreshScope(oauth); err == nil {
		t.Fatal("an OAuth-plane refresh token verified at the app plane's refreshSession")
	}
}

// Every token this bridge mints names its plane — session, rotation, and the
// access token inside each — so the verifiers below can refuse a token that
// names none.
func TestMintersStampTheAppPlane(t *testing.T) {
	m := testMinter(t, nil)
	tokens, err := m.MintSession(testLoginDID, testActorID, "alice", ScopeAppPass)
	if err != nil {
		t.Fatal(err)
	}
	rotated, err := m.MintRotation(testLoginDID, testActorID, "alice", ScopeAppPass, tokens.SessionID)
	if err != nil {
		t.Fatal(err)
	}
	for name, tok := range map[string]string{
		"session access": tokens.AccessJwt, "session refresh": tokens.RefreshJwt,
		"rotated access": rotated.AccessJwt, "rotated refresh": rotated.RefreshJwt,
	} {
		c, err := m.Verify(tok)
		if err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		if c.Plane != PlaneAppCredential {
			t.Errorf("%s: plane = %q, want %q", name, c.Plane, PlaneAppCredential)
		}
	}
}

// A token without a plane claim is refused at both verifiers: absence names no
// plane, and the plane is never guessed.
func TestAbsentPlaneClaimIsRefused(t *testing.T) {
	m := testMinter(t, nil)
	now := time.Now()
	base := Claims{
		Sub:        testLoginDID,
		Aud:        testServiceDID,
		Iat:        now.Unix(),
		Exp:        now.Add(time.Hour).Unix(),
		Jti:        b64([]byte("planeless-jti")),
		Sid:        b64([]byte("planeless-sid")),
		FaunaActor: strings.Repeat("ab", 32),
	}
	access := base
	access.Scope = ScopeAppPass
	accessJwt, err := m.sign(access)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := m.VerifyAccessScope(accessJwt); err == nil {
		t.Error("a plane-less access token verified")
	}
	refresh := base
	refresh.Scope = ScopeRefresh
	refreshJwt, err := m.sign(refresh)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := m.VerifyRefreshScope(refreshJwt); err == nil {
		t.Error("a plane-less refresh token verified")
	}
}
