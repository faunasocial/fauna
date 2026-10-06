package atprotopds

import (
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// stubAuthorizer stands in for the shared-Rust D8 module in this package's
// tests. It records the input it was handed and returns a verdict the test
// chose; it deliberately does NOT reimplement the matrix. The matrix lives in
// Rust with its own table-driven tests
// (libs/fauna-bridge-atproto/src/authz.rs), a Go copy would be the second
// decision point D8 exists to prevent, and the real binding is exercised
// end-to-end in cmd/fauna-atproto-bridge. What these tests prove is the
// *seat*: input assembly, verdict enforcement, and refusal uniformity.
//
// The one rule the default models is the kill-switch, because the plumbing
// from the flag cache to the module's input is this package's own invariant
// (F1's TestKillSwitchNudgeCutsLiveTokens rides on it).
type stubAuthorizer struct {
	mu     sync.Mutex
	last   AuthzInput
	calls  int
	decide func(AuthzInput) AuthzVerdict
}

func (s *stubAuthorizer) Authorize(in AuthzInput) AuthzVerdict {
	s.mu.Lock()
	s.last = in
	s.calls++
	decide := s.decide
	s.mu.Unlock()
	if decide != nil {
		return decide(in)
	}
	if !in.ExternalAppsEnabled {
		return AuthzVerdict{XrpcError: "AuthenticationRequired", Message: "authentication required"}
	}
	return AuthzVerdict{Allow: true}
}

func (s *stubAuthorizer) input() AuthzInput {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.last
}

func (s *stubAuthorizer) count() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.calls
}

// hookFixture drives the hook through a real route table, so the assertions
// cover the frame's actual middleware ordering rather than a direct call.
type hookFixture struct {
	srv   *Server
	authz *stubAuthorizer
	http  *httptest.Server
}

func newHookFixture(t *testing.T, routes ...xrpc.Route) *hookFixture {
	t.Helper()
	authz := &stubAuthorizer{}
	srv := NewServer(nil, nil, authz, nil, nil)
	caller := &xrpc.Caller{
		ActorID: make([]byte, 32),
		DID:     "did:fauna:00",
		Handle:  "alice",
		Scope:   ScopeAppPass,
		Plane:   PlaneAppCredential,
	}
	x := xrpc.NewServer(staticCaller{caller}, nil, srv.AuthzHook, nil, nil)
	for _, r := range routes {
		x.Register(r)
	}
	ts := httptest.NewServer(x)
	t.Cleanup(ts.Close)
	return &hookFixture{srv: srv, authz: authz, http: ts}
}

type staticCaller struct{ c *xrpc.Caller }

func (s staticCaller) VerifyAccess(*http.Request, string) (*xrpc.Caller, error) { return s.c, nil }

func okHandler(w http.ResponseWriter, _ *http.Request, _ *xrpc.Caller) {
	xrpc.WriteJSON(w, map[string]string{"ok": "yes"})
}

func (f *hookFixture) call(t *testing.T, nsid string, headers map[string]string) *http.Response {
	t.Helper()
	req, _ := http.NewRequest(http.MethodGet, f.http.URL+"/xrpc/"+nsid, nil)
	req.Header.Set("Authorization", "Bearer anything")
	for k, v := range headers {
		req.Header.Set(k, v)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { resp.Body.Close() })
	return resp
}

var authedRoute = xrpc.Route{
	NSID: "com.atproto.repo.getRecord", Method: http.MethodGet,
	Auth: xrpc.AppSession, Class: xrpc.ClassAuthed, Handle: okHandler,
}

var proxyRoute = xrpc.Route{
	NSID: "app.bsky.feed.getTimeline", Method: http.MethodGet,
	Auth: xrpc.AppSession, Class: xrpc.ClassAuthed, Proxyable: true, Handle: okHandler,
}

func TestHookAssemblesTheModuleInputFromRouteAndCaller(t *testing.T) {
	f := newHookFixture(t, authedRoute)
	if s := f.call(t, authedRoute.NSID, nil).StatusCode; s != http.StatusOK {
		t.Fatalf("allow verdict must reach the handler, got %d", s)
	}
	in := f.authz.input()
	if in.Plane != PlaneAppCredential {
		t.Errorf("plane = %q", in.Plane)
	}
	if len(in.Scopes) != 1 || in.Scopes[0] != ScopeAppPass {
		t.Errorf("scopes = %v", in.Scopes)
	}
	if in.Lxm != authedRoute.NSID {
		t.Errorf("lxm = %q, want the route NSID", in.Lxm)
	}
	if in.EndpointClass != "authed" {
		t.Errorf("endpoint_class = %q", in.EndpointClass)
	}
	if !in.ExternalAppsEnabled {
		t.Error("external_apps_enabled must default ON")
	}
	if in.Aud != nil {
		t.Errorf("aud must be nil off the proxy path, got %q", *in.Aud)
	}
}

// The OAuth plane's `scope` claim is space-delimited by spec; one splitter
// must serve both planes or F4 needs a second assembly path.
func TestHookSplitsASpaceDelimitedScopeClaim(t *testing.T) {
	f := newHookFixture(t, authedRoute)
	x := xrpc.NewServer(
		staticCaller{&xrpc.Caller{
			ActorID: make([]byte, 32),
			Scope:   "repo:* rpc:*?aud=* blob:*/*",
			Plane:   "oauth",
		}},
		nil, f.srv.AuthzHook, nil, nil,
	)
	x.Register(authedRoute)
	ts := httptest.NewServer(x)
	t.Cleanup(ts.Close)
	req, _ := http.NewRequest(http.MethodGet, ts.URL+"/xrpc/"+authedRoute.NSID, nil)
	req.Header.Set("Authorization", "Bearer anything")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if got := f.authz.input().Scopes; len(got) != 3 {
		t.Fatalf("scopes = %v, want 3 granular scopes", got)
	}
}

func TestHookEnforcesTheVerdictVerbatim(t *testing.T) {
	f := newHookFixture(t, authedRoute)
	f.authz.decide = func(AuthzInput) AuthzVerdict {
		return AuthzVerdict{XrpcError: "InvalidToken", Message: "no scope covers this"}
	}
	resp := f.call(t, authedRoute.NSID, nil)
	if resp.StatusCode != http.StatusForbidden {
		t.Fatalf("InvalidToken deny = %d, want 403", resp.StatusCode)
	}
}

// A deferred refusal must never read as permanent policy — it keeps the
// frame's MethodNotImplemented status and carries the module's "not yet".
func TestHookMapsADeferredDenyToMethodNotImplemented(t *testing.T) {
	f := newHookFixture(t, authedRoute)
	f.authz.decide = func(AuthzInput) AuthzVerdict {
		return AuthzVerdict{XrpcError: "MethodNotImplemented", Message: "not yet served"}
	}
	resp := f.call(t, authedRoute.NSID, nil)
	if resp.StatusCode != xrpc.MethodNotImplemented().Status {
		t.Fatalf("deferred deny = %d, want the frame's MethodNotImplemented status", resp.StatusCode)
	}
}

// The kill-switch refusal and a bad token must be indistinguishable.
func TestHookKillSwitchDenyIsUniformWithABadToken(t *testing.T) {
	f := newHookFixture(t, authedRoute)
	off := false
	f.srv.HandleSessionsChanged(make([]byte, 32), &off)
	resp := f.call(t, authedRoute.NSID, nil)
	if resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("kill-switch deny = %d, want 401", resp.StatusCode)
	}
	if in := f.authz.input(); in.ExternalAppsEnabled {
		t.Error("kill-switch OFF must arrive as external_apps_enabled=false")
	}
}

// Closed world extends to the seam: no module wired means no decision, which
// means refusal — never a silent allow.
func TestHookWithNoModuleRefuses(t *testing.T) {
	srv := NewServer(nil, nil, nil, nil, nil)
	x := xrpc.NewServer(
		staticCaller{&xrpc.Caller{ActorID: make([]byte, 32), Plane: PlaneAppCredential}},
		nil, srv.AuthzHook, nil, nil,
	)
	x.Register(authedRoute)
	ts := httptest.NewServer(x)
	t.Cleanup(ts.Close)
	req, _ := http.NewRequest(http.MethodGet, ts.URL+"/xrpc/"+authedRoute.NSID, nil)
	req.Header.Set("Authorization", "Bearer anything")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("no module wired = %d, want a refusal", resp.StatusCode)
	}
}

// A Public route has no plane, so there is nothing for D8 to decide — the
// route table's own declaration is its authorization, and the module must not
// even be consulted.
func TestHookSkipsPublicRoutes(t *testing.T) {
	public := xrpc.Route{
		NSID: "com.atproto.sync.getRepo", Method: http.MethodGet,
		Auth: xrpc.Public, Class: xrpc.ClassPublicRead, Handle: okHandler,
	}
	f := newHookFixture(t, public)
	if s := f.call(t, public.NSID, nil).StatusCode; s != http.StatusOK {
		t.Fatalf("public route = %d", s)
	}
	if f.authz.count() != 0 {
		t.Error("a public route must not consult the authorization module")
	}
}

// The atproto-proxy header is attacker-controlled. On a route the frame does
// not declare proxyable it must not reach the decision at all.
func TestForgedProxyHeaderIsIgnoredOnANonProxyableRoute(t *testing.T) {
	f := newHookFixture(t, authedRoute)
	f.call(t, authedRoute.NSID, map[string]string{"atproto-proxy": "did:web:api.bsky.chat#bsky_chat"})
	if aud := f.authz.input().Aud; aud != nil {
		t.Fatalf("aud = %q on a non-proxyable route; the forged header must be ignored", *aud)
	}
}

func TestProxyableRouteHandsTheTargetDidToTheModule(t *testing.T) {
	f := newHookFixture(t, proxyRoute)
	f.call(t, proxyRoute.NSID, map[string]string{"atproto-proxy": "did:web:api.bsky.chat#bsky_chat"})
	aud := f.authz.input().Aud
	if aud == nil || *aud != "did:web:api.bsky.chat#bsky_chat" {
		t.Fatalf("aud = %v, want the header's service DID", aud)
	}
}

func TestProxyableRouteWithNoHeaderHasNoAud(t *testing.T) {
	f := newHookFixture(t, proxyRoute)
	f.call(t, proxyRoute.NSID, nil)
	if aud := f.authz.input().Aud; aud != nil {
		t.Fatalf("aud = %q, want nil when the client sent no proxy header", *aud)
	}
}

// getServiceAuth runs the matrix twice — the route itself, then the requested
// method/audience — so a migration lxm stays deferred without a second policy
// implementation.
func TestAuthorizeServiceAuthRunsTheSecondCheck(t *testing.T) {
	authz := &stubAuthorizer{}
	srv := NewServer(nil, nil, authz, nil, nil)
	caller := &xrpc.Caller{ActorID: make([]byte, 32), Scope: ScopeAppPass, Plane: PlaneAppCredential}
	if e := srv.AuthorizeServiceAuth(caller, "com.atproto.repo.importRepo", "did:web:pds.example.com"); e != nil {
		t.Fatalf("stub allows, so the seat must allow: %v", e)
	}
	in := authz.input()
	if in.Lxm != "com.atproto.repo.importRepo" {
		t.Errorf("lxm = %q, want the REQUESTED method", in.Lxm)
	}
	if in.Aud == nil || *in.Aud != "did:web:pds.example.com" {
		t.Errorf("aud = %v, want the requested audience", in.Aud)
	}
}

func TestAuthorizeServiceAuthWithNoModuleRefuses(t *testing.T) {
	srv := NewServer(nil, nil, nil, nil, nil)
	caller := &xrpc.Caller{ActorID: make([]byte, 32), Plane: PlaneAppCredential}
	if e := srv.AuthorizeServiceAuth(caller, "app.bsky.feed.getTimeline", "did:web:api.bsky.app"); e == nil {
		t.Fatal("no module wired must refuse")
	}
}

// The class names are a cross-language contract with the Rust module; an
// unknown value must stringify to "" so the module's closed world denies.
func TestEndpointClassNamesMatchTheModule(t *testing.T) {
	for _, tc := range []struct {
		class xrpc.EndpointClass
		want  string
	}{
		{xrpc.ClassAuth, "auth"},
		{xrpc.ClassPublicRead, "public_read"},
		{xrpc.ClassAuthed, "authed"},
		{xrpc.ClassWrite, "write"},
		{xrpc.ClassBlob, "blob"},
		{xrpc.EndpointClass(99), ""},
	} {
		if got := tc.class.String(); got != tc.want {
			t.Errorf("class %d = %q, want %q", tc.class, got, tc.want)
		}
	}
}
