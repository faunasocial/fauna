package xrpc

import (
	"bytes"
	"fmt"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

func TestIPLimiterWindows(t *testing.T) {
	now := time.Unix(1_760_000_000, 0)
	l := NewIPLimiter(func() time.Time { return now })

	// ClassAuth: 10 / 5 min.
	for i := 0; i < 10; i++ {
		if !l.Allow("1.2.3.4", ClassAuth) {
			t.Fatalf("auth request %d refused inside the window", i)
		}
	}
	if l.Allow("1.2.3.4", ClassAuth) {
		t.Fatal("11th auth request allowed")
	}
	// A different IP has its own bucket; a different class too.
	if !l.Allow("5.6.7.8", ClassAuth) {
		t.Fatal("other IP shares the bucket")
	}
	if !l.Allow("1.2.3.4", ClassPublicRead) {
		t.Fatal("other class shares the bucket")
	}
	// The window resets.
	now = now.Add(5*time.Minute + time.Second)
	if !l.Allow("1.2.3.4", ClassAuth) {
		t.Fatal("window did not reset")
	}
}

// The per-IP ClassAuth rejection is the ONE rate-limit exit that logs, and it
// logs the real source_ip. That log line is the tier_4 SNI-router proof's
// observable (`test_atproto_pds_sni_router.py`): behind the router it carries
// the PROXY-v2-conveyed client IP, so a loopback value there means the router's
// `--send-proxy-to` header never reached the listener. This headless test pins
// the mechanism (the 11th same-IP createSession is refused AND logs its IP)
// independent of the heavy tier_4 docker build.
func TestSourceIPRateLimitRejectionLogsSourceIP(t *testing.T) {
	var buf bytes.Buffer
	logger := slog.New(slog.NewTextHandler(&buf, &slog.HandlerOptions{Level: slog.LevelWarn}))
	now := time.Unix(1_760_000_000, 0)
	limiter := NewIPLimiter(func() time.Time { return now })
	s := NewServer(nil, nil, nil, limiter, logger)
	s.Register(Route{
		NSID: "com.atproto.server.createSession", Method: http.MethodPost,
		Auth: Public, Class: ClassAuth,
		Handle: func(w http.ResponseWriter, _ *http.Request, _ *Caller) { WriteJSON(w, map[string]any{}) },
	})

	const clientIP = "203.0.113.7"
	do := func() int {
		req := httptest.NewRequest(http.MethodPost, "/xrpc/com.atproto.server.createSession", nil)
		req.RemoteAddr = clientIP + ":40000"
		rec := httptest.NewRecorder()
		s.ServeHTTP(rec, req)
		return rec.Code
	}
	// 10 allowed inside the window (silent); the 11th is refused and logged.
	for i := 0; i < 10; i++ {
		if c := do(); c != http.StatusOK {
			t.Fatalf("request %d refused inside the window: %d", i, c)
		}
	}
	if c := do(); c != http.StatusTooManyRequests {
		t.Fatalf("11th same-IP request not rate-limited: %d", c)
	}
	logged := buf.String()
	if strings.Count(logged, "rate limit exceeded") != 1 {
		t.Fatalf("expected exactly one rate-limit log line (only the 429 exit logs); got: %q", logged)
	}
	if !strings.Contains(logged, "source_ip="+clientIP) {
		t.Fatalf("rejection log missing the real source_ip=%s; got: %q", clientIP, logged)
	}
}

func TestDuplicateRoutePanics(t *testing.T) {
	s := NewServer(nil, nil, nil, nil, nil)
	route := Route{NSID: "com.example.method", Method: http.MethodGet, Handle: func(http.ResponseWriter, *http.Request, *Caller) {}}
	s.Register(route)
	defer func() {
		if recover() == nil {
			t.Fatal("duplicate route registration did not panic")
		}
	}()
	s.Register(route)
}

type staticVerifier struct{ caller *Caller }

func (v staticVerifier) VerifyAccess(_ *http.Request, token string) (*Caller, error) {
	if token == "good" && v.caller != nil {
		return v.caller, nil
	}
	return nil, errBadToken
}

func TestPathParsingAndAuthClassMatrix(t *testing.T) {
	caller := &Caller{DID: "did:fauna:aa", ActorID: make([]byte, 32)}
	// A permissive hook: this test is about the auth-class matrix, not about
	// what happens with no authorization installed — that case is its own test
	// (TestAnAuthedRouteWithNoAuthzHookRefuses) and it refuses.
	allowAll := func(*http.Request, *Route, *Caller) *Error { return nil }
	s := NewServer(staticVerifier{caller}, nil, allowAll, nil, nil)
	var sawCaller *Caller
	s.Register(Route{NSID: "com.example.public", Method: http.MethodGet, Auth: Public,
		Handle: func(w http.ResponseWriter, _ *http.Request, c *Caller) { sawCaller = c; WriteJSON(w, map[string]any{}) }})
	s.Register(Route{NSID: "com.example.authed", Method: http.MethodGet, Auth: AppSession,
		Handle: func(w http.ResponseWriter, _ *http.Request, c *Caller) { sawCaller = c; WriteJSON(w, map[string]any{}) }})
	// OAuthSession with no verifier installed (F4 not landed): refuses.
	s.Register(Route{NSID: "com.example.oauth", Method: http.MethodGet, Auth: OAuthSession,
		Handle: func(w http.ResponseWriter, _ *http.Request, _ *Caller) { WriteJSON(w, map[string]any{}) }})

	do := func(path, bearer string) int {
		req := httptest.NewRequest(http.MethodGet, path, nil)
		if bearer != "" {
			req.Header.Set("Authorization", "Bearer "+bearer)
		}
		rec := httptest.NewRecorder()
		s.ServeHTTP(rec, req)
		return rec.Code
	}

	if c := do("/notxrpc/com.example.public", ""); c != http.StatusBadRequest {
		t.Fatalf("non-xrpc path: %d", c)
	}
	if c := do("/xrpc/com.example.public/extra", ""); c != http.StatusBadRequest {
		t.Fatalf("slash in nsid: %d", c)
	}
	if c := do("/xrpc/com.example.public", ""); c != http.StatusOK {
		t.Fatalf("public route: %d", c)
	}
	if sawCaller != nil {
		t.Fatal("public route saw a caller")
	}
	if c := do("/xrpc/com.example.authed", ""); c != http.StatusUnauthorized {
		t.Fatalf("authed without token: %d", c)
	}
	if c := do("/xrpc/com.example.authed", "bad"); c != http.StatusUnauthorized {
		t.Fatalf("authed with bad token: %d", c)
	}
	if c := do("/xrpc/com.example.authed", "good"); c != http.StatusOK {
		t.Fatalf("authed with good token: %d", c)
	}
	if sawCaller != caller {
		t.Fatal("authed route did not receive the verified caller")
	}
	if c := do("/xrpc/com.example.oauth", "good"); c != http.StatusUnauthorized {
		t.Fatalf("oauth route without a live oauth verifier: %d", c)
	}
}

// The frame's closed world must extend to the frame itself: with no authz hook
// installed there is no authorization decision to make, so an authenticated
// route must refuse rather than serve on authn alone. D8's whole point is one
// decision point, never zero.
func TestAnAuthedRouteWithNoAuthzHookRefuses(t *testing.T) {
	caller := &Caller{DID: "did:fauna:aa", ActorID: make([]byte, 32)}
	s := NewServer(staticVerifier{caller}, nil, nil /* no authz hook */, nil, nil)
	ok := func(w http.ResponseWriter, _ *http.Request, _ *Caller) { WriteJSON(w, map[string]any{}) }
	s.Register(Route{NSID: "com.example.authed", Method: http.MethodGet, Auth: AppSession, Handle: ok})
	s.Register(Route{NSID: "com.example.public", Method: http.MethodGet, Auth: Public, Handle: ok})

	do := func(path string) int {
		req := httptest.NewRequest(http.MethodGet, path, nil)
		req.Header.Set("Authorization", "Bearer good")
		rec := httptest.NewRecorder()
		s.ServeHTTP(rec, req)
		return rec.Code
	}
	if c := do("/xrpc/com.example.authed"); c != http.StatusUnauthorized {
		t.Fatalf("authed route served with no authorization installed: %d", c)
	}
	// Public routes are unaffected: `Auth: Public` in the route table IS their
	// authorization, so there is nothing for a hook to decide.
	if c := do("/xrpc/com.example.public"); c != http.StatusOK {
		t.Fatalf("public route wrongly refused: %d", c)
	}
}

func TestAuthzHookRefusal(t *testing.T) {
	caller := &Caller{DID: "did:fauna:aa"}
	hook := func(_ *http.Request, route *Route, c *Caller) *Error {
		if route.NSID == "com.example.blocked" {
			return AuthRequired()
		}
		return nil
	}
	s := NewServer(staticVerifier{caller}, nil, hook, nil, nil)
	ok := func(w http.ResponseWriter, _ *http.Request, _ *Caller) { WriteJSON(w, map[string]any{}) }
	s.Register(Route{NSID: "com.example.blocked", Method: http.MethodGet, Auth: AppSession, Handle: ok})
	s.Register(Route{NSID: "com.example.open", Method: http.MethodGet, Auth: AppSession, Handle: ok})

	do := func(path string) int {
		req := httptest.NewRequest(http.MethodGet, path, nil)
		req.Header.Set("Authorization", "Bearer good")
		rec := httptest.NewRecorder()
		s.ServeHTTP(rec, req)
		return rec.Code
	}
	if c := do("/xrpc/com.example.blocked"); c != http.StatusUnauthorized {
		t.Fatalf("authz hook did not refuse: %d", c)
	}
	if c := do("/xrpc/com.example.open"); c != http.StatusOK {
		t.Fatalf("authz hook over-blocked: %d", c)
	}
}

// **⚠ The scheme selects the plane, and a bound token presented as `Bearer` is
// refused STRUCTURALLY** (F4 slice 7).
//
// An app-credential token is a bearer credential by design; an OAuth access
// token is DPoP-bound and arrives as `DPoP`. The obvious "be liberal, try both
// verifiers" shape would silently accept every OAuth token as a bearer
// credential — discarding the key binding the whole DPoP plane exists to
// provide, with nothing failing anywhere to say so. This pins that the Bearer
// branch never reaches the OAuth verifier and vice versa.
//
// Both verifiers accept the same token string here on purpose: if the routing
// were by "try each until one works", every assertion below would pass while
// the property was gone. The subject is WHICH verifier is consulted, so the
// test makes the token itself carry no information at all.
func TestTheAuthorizationSchemeSelectsThePlane(t *testing.T) {
	appCaller := &Caller{DID: "did:plc:app", ActorID: make([]byte, 32), Plane: "app_credential"}
	oauthCaller := &Caller{DID: "did:plc:oauth", ActorID: make([]byte, 32), Plane: "oauth"}
	allowAll := func(*http.Request, *Route, *Caller) *Error { return nil }
	s := NewServer(staticVerifier{appCaller}, staticVerifier{oauthCaller}, allowAll, nil, nil)

	var sawCaller *Caller
	s.Register(Route{NSID: "com.example.either", Method: http.MethodGet, Auth: Session,
		Handle: func(w http.ResponseWriter, _ *http.Request, c *Caller) {
			sawCaller = c
			WriteJSON(w, map[string]any{})
		}})

	do := func(scheme string) (int, *Caller) {
		sawCaller = nil
		req := httptest.NewRequest(http.MethodGet, "/xrpc/com.example.either", nil)
		req.Header.Set("Authorization", scheme+" good")
		rec := httptest.NewRecorder()
		s.ServeHTTP(rec, req)
		return rec.Code, sawCaller
	}

	code, caller := do("Bearer")
	if code != http.StatusOK || caller == nil || caller.Plane != "app_credential" {
		t.Errorf("Bearer resolved to %+v (status %d), want the APP plane", caller, code)
	}
	code, caller = do("DPoP")
	if code != http.StatusOK || caller == nil || caller.Plane != "oauth" {
		t.Errorf("DPoP resolved to %+v (status %d), want the OAUTH plane", caller, code)
	}

	// An unknown scheme is no token at all, never a fallback into either plane.
	req := httptest.NewRequest(http.MethodGet, "/xrpc/com.example.either", nil)
	req.Header.Set("Authorization", "Basic good")
	rec := httptest.NewRecorder()
	s.ServeHTTP(rec, req)
	if rec.Code == http.StatusOK {
		t.Error("an unrecognised authorization scheme must not authenticate")
	}
}

// The app plane must not be reachable over the DPoP scheme either: an app
// credential is not DPoP-bound, so honouring one there would let a caller claim
// a binding it never made.
func TestAnAppCredentialIsNotAcceptedOverTheDPoPScheme(t *testing.T) {
	appCaller := &Caller{DID: "did:plc:app", ActorID: make([]byte, 32), Plane: "app_credential"}
	allowAll := func(*http.Request, *Route, *Caller) *Error { return nil }
	// No OAuth verifier installed — the state every bridge was in before F4.
	s := NewServer(staticVerifier{appCaller}, nil, allowAll, nil, nil)
	s.Register(Route{NSID: "com.example.authed", Method: http.MethodGet, Auth: AppSession,
		Handle: func(w http.ResponseWriter, _ *http.Request, _ *Caller) { WriteJSON(w, map[string]any{}) }})

	req := httptest.NewRequest(http.MethodGet, "/xrpc/com.example.authed", nil)
	req.Header.Set("Authorization", "DPoP good")
	rec := httptest.NewRecorder()
	s.ServeHTTP(rec, req)
	if rec.Code == http.StatusOK {
		t.Error("an app-credential token presented as DPoP must be refused")
	}
}

// ── The DPoP plane's nonce duty ──────────────────────────────

// nonceVerifier is a DPoP-plane verifier that both requires and issues nonces:
// `"good"` authenticates, `"stale"` is the retryable refusal, anything else is
// an ordinary one. Each mint is distinct so a test can tell a freshly issued
// nonce from a header left over on the recorder.
type nonceVerifier struct {
	caller *Caller
	minted int
}

func (v *nonceVerifier) VerifyAccess(_ *http.Request, token string) (*Caller, error) {
	switch token {
	case "good":
		return v.caller, nil
	case "stale":
		return nil, ErrUseDPoPNonce
	default:
		return nil, errBadToken
	}
}

func (v *nonceVerifier) IssueNonce() string {
	v.minted++
	return fmt.Sprintf("nonce-%d", v.minted)
}

// **The plane that REQUIRES a nonce must ISSUE one on every response** —
// success and every flavour of refusal alike.
//
// This is the pin for finding, whose whole shape was that the
// requirement shipped without the duty: `VerifyAccess` demanded a live nonce
// while no XRPC response carried one, so a client was refused as soon as the
// nonce it had harvested from an authorization-server endpoint aged out. The
// outcomes are enumerated deliberately rather than sampled — a fix that set the
// header only on the happy path, or only on the nonce refusal, would leave a
// client stuck on whichever response it actually received, and would make the
// header's presence a distinguisher between refusals.
func TestEveryResponseOnTheDPoPPlaneCarriesAFreshNonce(t *testing.T) {
	caller := &Caller{DID: "did:plc:oauth", ActorID: make([]byte, 32), Plane: "oauth"}
	verifier := &nonceVerifier{caller: caller}
	refuseAuthz := false
	hook := func(*http.Request, *Route, *Caller) *Error {
		if refuseAuthz {
			return &Error{Status: http.StatusForbidden, Name: "Forbidden", Message: "no"}
		}
		return nil
	}
	limiter := NewIPLimiter(func() time.Time { return time.Unix(1_760_000_000, 0) })
	s := NewServer(staticVerifier{caller}, verifier, hook, limiter, nil)
	s.Register(Route{NSID: "com.example.oauth", Method: http.MethodGet, Auth: Session,
		Class: ClassAuth,
		Handle: func(w http.ResponseWriter, _ *http.Request, _ *Caller) {
			WriteJSON(w, map[string]any{"ok": true})
		}})

	get := func(path, token string) *httptest.ResponseRecorder {
		req := httptest.NewRequest(http.MethodGet, path, nil)
		req.Header.Set("Authorization", "DPoP "+token)
		rec := httptest.NewRecorder()
		s.ServeHTTP(rec, req)
		return rec
	}

	const path = "/xrpc/com.example.oauth"
	for _, tc := range []struct {
		name   string
		run    func() *httptest.ResponseRecorder
		status int
	}{
		{"an authenticated success", func() *httptest.ResponseRecorder {
			return get(path, "good")
		}, http.StatusOK},
		{"the retryable nonce refusal", func() *httptest.ResponseRecorder {
			return get(path, "stale")
		}, http.StatusUnauthorized},
		{"an ordinary auth refusal", func() *httptest.ResponseRecorder {
			return get(path, "forged")
		}, http.StatusUnauthorized},
		{"an authorization refusal", func() *httptest.ResponseRecorder {
			refuseAuthz = true
			defer func() { refuseAuthz = false }()
			return get(path, "good")
		}, http.StatusForbidden},
		{"an unknown method", func() *httptest.ResponseRecorder {
			return get("/xrpc/com.example.nope", "good")
		}, http.StatusNotFound},
		{"a wrong HTTP method", func() *httptest.ResponseRecorder {
			req := httptest.NewRequest(http.MethodPost, path, nil)
			req.Header.Set("Authorization", "DPoP good")
			rec := httptest.NewRecorder()
			s.ServeHTTP(rec, req)
			return rec
		}, http.StatusBadRequest},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rec := tc.run()
			if rec.Code != tc.status {
				t.Fatalf("status = %d, want %d (the case is misbuilt if this fails)", rec.Code, tc.status)
			}
			if got := rec.Header().Get("DPoP-Nonce"); got == "" {
				t.Errorf("no DPoP-Nonce on %s — a client that receives this response "+
					"has no way to learn a fresh nonce, which is finding exactly", tc.name)
			}
		})
	}

	// The rate-limited answer too: the nonce is owed to the credential the
	// request presents, and a 429 is the one response a client under pressure
	// is most likely to be holding when its nonce ages out.
	t.Run("a rate-limited refusal", func(t *testing.T) {
		var rec *httptest.ResponseRecorder
		for i := 0; i < 12; i++ {
			rec = get(path, "good")
		}
		if rec.Code != http.StatusTooManyRequests {
			t.Fatalf("status = %d, want 429 — the ClassAuth budget is 10/5min", rec.Code)
		}
		if rec.Header().Get("DPoP-Nonce") == "" {
			t.Error("no DPoP-Nonce on the rate-limited refusal")
		}
	})
}

// The duty is the DPoP plane's alone — a bearer credential neither requires a
// nonce nor gets one, and an unauthenticated public read stays a plain
// response. Minting for every caller would put an unasked-for header on the
// whole mirror read surface.
func TestOnlyTheDPoPPlaneIsIssuedANonce(t *testing.T) {
	caller := &Caller{DID: "did:plc:app", ActorID: make([]byte, 32), Plane: "app_credential"}
	verifier := &nonceVerifier{caller: caller}
	allowAll := func(*http.Request, *Route, *Caller) *Error { return nil }
	s := NewServer(staticVerifier{caller}, verifier, allowAll, nil, nil)
	s.Register(Route{NSID: "com.example.either", Method: http.MethodGet, Auth: Session,
		Handle: func(w http.ResponseWriter, _ *http.Request, _ *Caller) { WriteJSON(w, map[string]any{}) }})
	s.Register(Route{NSID: "com.example.open", Method: http.MethodGet, Auth: Public,
		Handle: func(w http.ResponseWriter, _ *http.Request, _ *Caller) { WriteJSON(w, map[string]any{}) }})

	for _, tc := range []struct{ name, path, authorization string }{
		{"a bearer credential", "/xrpc/com.example.either", "Bearer good"},
		{"an unrecognised scheme", "/xrpc/com.example.either", "Basic good"},
		{"a public read with no token", "/xrpc/com.example.open", ""},
	} {
		t.Run(tc.name, func(t *testing.T) {
			req := httptest.NewRequest(http.MethodGet, tc.path, nil)
			if tc.authorization != "" {
				req.Header.Set("Authorization", tc.authorization)
			}
			rec := httptest.NewRecorder()
			s.ServeHTTP(rec, req)
			if got := rec.Header().Get("DPoP-Nonce"); got != "" {
				t.Errorf("DPoP-Nonce = %q on %s — the header is the DPoP plane's", got, tc.name)
			}
		})
	}
	if verifier.minted != 0 {
		t.Errorf("minted %d nonce(s) for non-DPoP traffic, want 0", verifier.minted)
	}
}

// **The challenge is the ONE refusal detail that crosses the wire, and the body
// stays byte-identical.**
//
// Item 2 of the contract, and the half that is easy to get wrong in the
// generous direction: a client cannot act on a refusal it cannot recognise, but
// widening the uniform-failure rule any further than this one retryable case
// would hand back the enumeration signal the frame exists to deny. So a forged
// token and a stale nonce must differ in exactly one header and nothing else.
func TestOnlyTheNonceRefusalCarriesAChallengeAndTheBodyNeverDiffers(t *testing.T) {
	caller := &Caller{DID: "did:plc:oauth", ActorID: make([]byte, 32), Plane: "oauth"}
	allowAll := func(*http.Request, *Route, *Caller) *Error { return nil }
	s := NewServer(nil, &nonceVerifier{caller: caller}, allowAll, nil, nil)
	s.Register(Route{NSID: "com.example.oauth", Method: http.MethodGet, Auth: OAuthSession,
		Handle: func(w http.ResponseWriter, _ *http.Request, _ *Caller) { WriteJSON(w, map[string]any{}) }})

	refuse := func(token string) *httptest.ResponseRecorder {
		req := httptest.NewRequest(http.MethodGet, "/xrpc/com.example.oauth", nil)
		req.Header.Set("Authorization", "DPoP "+token)
		rec := httptest.NewRecorder()
		s.ServeHTTP(rec, req)
		return rec
	}

	stale, forged := refuse("stale"), refuse("forged")
	if stale.Code != http.StatusUnauthorized || forged.Code != http.StatusUnauthorized {
		t.Fatalf("statuses = %d/%d, want 401 for both", stale.Code, forged.Code)
	}
	if got, want := stale.Header().Get("WWW-Authenticate"), `DPoP error="use_dpop_nonce"`; got != want {
		t.Errorf("challenge = %q, want %q — without it the client cannot know the "+
			"call is retryable and reads a permanent failure", got, want)
	}
	if got := forged.Header().Get("WWW-Authenticate"); got != "" {
		t.Errorf("a forged token was answered with %q — every refusal but the "+
			"nonce one must stay uniform", got)
	}
	if stale.Body.String() != forged.Body.String() {
		t.Errorf("bodies differ (%q vs %q) — the challenge belongs in the header; "+
			"the body is what must never distinguish one refusal from another",
			stale.Body.String(), forged.Body.String())
	}
}

// A bearer credential must never be able to reach the challenge branch: the app
// plane has no nonce, so an `ErrUseDPoPNonce` arriving from the app verifier
// would be a plane confusion, and answering it would advertise a DPoP
// requirement on a plane that has none.
func TestTheAppPlaneCanNeverProduceANonceChallenge(t *testing.T) {
	allowAll := func(*http.Request, *Route, *Caller) *Error { return nil }
	// An app verifier that (wrongly) returns the DPoP plane's sentinel.
	s := NewServer(nonceClaimingVerifier{}, nil, allowAll, nil, nil)
	s.Register(Route{NSID: "com.example.app", Method: http.MethodGet, Auth: AppSession,
		Handle: func(w http.ResponseWriter, _ *http.Request, _ *Caller) { WriteJSON(w, map[string]any{}) }})

	req := httptest.NewRequest(http.MethodGet, "/xrpc/com.example.app", nil)
	req.Header.Set("Authorization", "Bearer whatever")
	rec := httptest.NewRecorder()
	s.ServeHTTP(rec, req)
	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("status = %d, want 401", rec.Code)
	}
	if got := rec.Header().Get("WWW-Authenticate"); got != "" {
		t.Errorf("the app plane produced a %q challenge — the sentinel must be "+
			"unreachable from the Bearer branch", got)
	}
}

type nonceClaimingVerifier struct{}

func (nonceClaimingVerifier) VerifyAccess(*http.Request, string) (*Caller, error) {
	return nil, ErrUseDPoPNonce
}
