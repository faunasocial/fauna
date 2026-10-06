package atprotopds

// Tests for the F3 service-proxy path (proxy.go), driven through the REAL
// frame — fallback, middleware chain, D8 seat — against an httptest upstream.
// The guard/pin/cap mechanics are safefetch's own tests; the shared-Rust
// policy has its Rust tests plus the cross-language proof in
// cmd/fauna-atproto-bridge. What THIS file proves is the composition: which
// requests become forwards, what the forward carries, and what comes back.

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/safefetch"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// testAppViewDID stands in for the FFI-carried authz::APPVIEW_SERVICE_DID.
// The value is the same string on purpose: the assertions below check it
// flows to BOTH D8's aud input and the minted token.
const testAppViewDID = "did:web:api.bsky.app#bsky_appview"

type stubResolver struct {
	mu       sync.Mutex
	endpoint string
	err      error
	gotDID   string
	gotFrag  string
	calls    int
}

func (s *stubResolver) ResolveEndpoint(_ context.Context, did, fragment string) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.calls++
	s.gotDID, s.gotFrag = did, fragment
	if s.err != nil {
		return "", s.err
	}
	return s.endpoint, nil
}

// plainFetcher performs the forward with a plain HTTP client — the upstream
// here is a local httptest server, and the guard mechanics are proven in
// safefetch's own tests.
type plainFetcher struct{}

func (plainFetcher) DoStream(ctx context.Context, req safefetch.Request) (*http.Response, error) {
	hreq, err := http.NewRequestWithContext(ctx, req.Method, req.URL, req.Body)
	if err != nil {
		return nil, err
	}
	for k, vs := range req.Header {
		for _, v := range vs {
			hreq.Header.Add(k, v)
		}
	}
	return http.DefaultClient.Do(hreq)
}

type errFetcher struct{ err error }

func (e errFetcher) DoStream(context.Context, safefetch.Request) (*http.Response, error) {
	return nil, e.err
}

// upstreamRecorder is the fake AppView: it records what arrived and answers a
// canned timeline.
type upstreamRecorder struct {
	mu    sync.Mutex
	last  *http.Request
	body  []byte
	calls int
}

func (u *upstreamRecorder) handler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		u.mu.Lock()
		u.last = r.Clone(context.Background())
		u.body = body
		u.calls++
		u.mu.Unlock()
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("Atproto-Repo-Rev", "rev-7")
		_, _ = w.Write([]byte(`{"feed":[{"post":"fixture"}]}`))
	})
}

func (u *upstreamRecorder) count() int {
	u.mu.Lock()
	defer u.mu.Unlock()
	return u.calls
}

func (u *upstreamRecorder) request() (*http.Request, []byte) {
	u.mu.Lock()
	defer u.mu.Unlock()
	return u.last, u.body
}

type proxyFixture struct {
	srv      *Server
	authz    *stubAuthorizer
	resolver *stubResolver
	upstream *upstreamRecorder
	http     *httptest.Server
}

// newProxyFixture assembles the production shape: frame + fallback + enabled
// proxy, a real K-256 test signer, an allow-all D8 stub, and a recording
// upstream the stub resolver points at.
func newProxyFixture(t *testing.T, routes ...xrpc.Route) *proxyFixture {
	t.Helper()
	upstream := &upstreamRecorder{}
	up := httptest.NewServer(upstream.handler())
	t.Cleanup(up.Close)

	signer, _ := testSigner(t)
	authz := &stubAuthorizer{}
	resolver := &stubResolver{endpoint: up.URL}
	srv := NewServer(nil, nil, authz, &stubSigners{signer: signer}, nil)
	srv.flags.set(make([]byte, 32), true)
	srv.EnableProxy(ProxyConfig{
		AppViewDID: testAppViewDID,
		Resolver:   resolver,
		Fetch:      plainFetcher{},
	})

	caller := &xrpc.Caller{
		ActorID: make([]byte, 32),
		DID:     "did:fauna:alice",
		Handle:  "alice",
		Scope:   ScopeAppPass,
		Plane:   PlaneAppCredential,
	}
	x := xrpc.NewServer(staticCaller{caller}, nil, srv.AuthzHook, nil, nil)
	for _, r := range routes {
		x.Register(r)
	}
	x.SetFallback(srv.ProxyFallback)
	ts := httptest.NewServer(x)
	t.Cleanup(ts.Close)
	return &proxyFixture{srv: srv, authz: authz, resolver: resolver, upstream: upstream, http: ts}
}

func (f *proxyFixture) do(t *testing.T, method, path string, headers map[string]string, body io.Reader) *http.Response {
	t.Helper()
	req, err := http.NewRequest(method, f.http.URL+path, body)
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Authorization", "Bearer client-access-token")
	for k, v := range headers {
		req.Header.Set(k, v)
	}
	resp, err := f.http.Client().Do(req)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { resp.Body.Close() })
	return resp
}

// The LEAD's success shape in miniature: an unregistered app.bsky.* read with
// no header forwards to the AppView default with a freshly minted service JWT
// and streams the timeline back.
func TestHeaderlessAppBskyReadForwardsToTheAppView(t *testing.T) {
	f := newProxyFixture(t)
	resp := f.do(t, http.MethodGet, "/xrpc/app.bsky.feed.getTimeline?limit=10", nil, nil)

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status = %d", resp.StatusCode)
	}
	body, _ := io.ReadAll(resp.Body)
	if !strings.Contains(string(body), `"fixture"`) {
		t.Fatalf("body = %q, want the upstream timeline streamed back", body)
	}
	if ct := resp.Header.Get("Content-Type"); ct != "application/json" {
		t.Errorf("Content-Type = %q, want the upstream's", ct)
	}
	if rev := resp.Header.Get("Atproto-Repo-Rev"); rev != "rev-7" {
		t.Errorf("Atproto-Repo-Rev = %q, want forwarded", rev)
	}

	// D8 authorized against the SAME audience the forward dialed — the drift
	// this constant crossing the FFI exists to prevent.
	if aud := f.authz.input().Aud; aud == nil || *aud != testAppViewDID {
		t.Errorf("D8 aud = %v, want the AppView default", aud)
	}
	if f.resolver.gotDID != "did:web:api.bsky.app" || f.resolver.gotFrag != "bsky_appview" {
		t.Errorf("resolved %q#%q, want the AppView ref split", f.resolver.gotDID, f.resolver.gotFrag)
	}

	up, _ := f.upstream.request()
	if up.URL.Path != "/xrpc/app.bsky.feed.getTimeline" {
		t.Errorf("upstream path = %q", up.URL.Path)
	}
	if up.URL.RawQuery != "limit=10" {
		t.Errorf("upstream query = %q, want the caller's forwarded", up.URL.RawQuery)
	}

	// The Authorization the upstream saw is a minted service JWT — never the
	// caller's PDS access token.
	authz := up.Header.Get("Authorization")
	if strings.Contains(authz, "client-access-token") {
		t.Fatal("the caller's access token was forwarded upstream")
	}
	token := strings.TrimPrefix(authz, "Bearer ")
	_, claims, _, _ := decodeJWT(t, token)
	if claims["iss"] != "did:fauna:alice" {
		t.Errorf("iss = %v", claims["iss"])
	}
	if claims["aud"] != testAppViewDID {
		t.Errorf("aud = %v, want the dialed audience", claims["aud"])
	}
	if claims["lxm"] != "app.bsky.feed.getTimeline" {
		t.Errorf("lxm = %v, want the forwarded method", claims["lxm"])
	}
	exp, ok := claims["exp"].(float64)
	if !ok || time.Until(time.Unix(int64(exp), 0)) > MaxServiceAuthLifetime+time.Second {
		t.Errorf("exp = %v, want within the 60 s cap", claims["exp"])
	}
}

func TestExplicitProxyHeaderWinsOverTheAppViewDefault(t *testing.T) {
	f := newProxyFixture(t)
	const chat = "did:web:api.bsky.chat#bsky_chat"
	resp := f.do(t, http.MethodGet, "/xrpc/app.bsky.feed.getTimeline",
		map[string]string{"atproto-proxy": chat}, nil)
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status = %d", resp.StatusCode)
	}
	if aud := f.authz.input().Aud; aud == nil || *aud != chat {
		t.Errorf("D8 aud = %v, want the header's ref", aud)
	}
	if f.resolver.gotDID != "did:web:api.bsky.chat" {
		t.Errorf("resolved %q, want the header's DID", f.resolver.gotDID)
	}
	up, _ := f.upstream.request()
	_, claims, _, _ := decodeJWT(t, strings.TrimPrefix(up.Header.Get("Authorization"), "Bearer "))
	if claims["aud"] != chat {
		t.Errorf("minted aud = %v, want the header's ref", claims["aud"])
	}
}

func TestAnUnknownNonAppBskyMethodWithoutAHeaderIsNotImplemented(t *testing.T) {
	f := newProxyFixture(t)
	resp := f.do(t, http.MethodGet, "/xrpc/com.example.custom.method", nil, nil)
	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("status = %d, want the closed-world 404", resp.StatusCode)
	}
	if f.upstream.count() != 0 {
		t.Fatal("nothing may have been forwarded")
	}
}

// Preferences hold PDS-private state and are served locally (phase 4) — the
// headerless default must never leak them to a third party in the meantime.
func TestPreferencesAreNeverForwardedByTheHeaderlessDefault(t *testing.T) {
	f := newProxyFixture(t)
	for _, nsid := range []string{"app.bsky.actor.getPreferences", "app.bsky.actor.putPreferences"} {
		method := http.MethodGet
		if strings.HasPrefix(nsid, "app.bsky.actor.put") {
			method = http.MethodPost
		}
		resp := f.do(t, method, "/xrpc/"+nsid, nil, nil)
		if resp.StatusCode != http.StatusNotFound {
			t.Errorf("%s: status = %d, want 404 until phase 4 serves it locally", nsid, resp.StatusCode)
		}
	}
	if f.upstream.count() != 0 {
		t.Fatal("a preferences call reached the upstream")
	}
}

func TestARegisteredRouteIsNeverShadowedByTheFallback(t *testing.T) {
	f := newProxyFixture(t, xrpc.Route{
		NSID: "app.bsky.feed.getTimeline", Method: http.MethodGet,
		Auth: xrpc.AppSession, Class: xrpc.ClassAuthed, Handle: okHandler,
	})
	resp := f.do(t, http.MethodGet, "/xrpc/app.bsky.feed.getTimeline",
		map[string]string{"atproto-proxy": testAppViewDID}, nil)
	body, _ := io.ReadAll(resp.Body)
	if !strings.Contains(string(body), `"ok"`) {
		t.Fatalf("body = %q, want the LOCAL handler's reply", body)
	}
	if f.upstream.count() != 0 {
		t.Fatal("a locally served method was forwarded")
	}
}

func TestTheFallbackRefusesNonXrpcVerbs(t *testing.T) {
	f := newProxyFixture(t)
	resp := f.do(t, http.MethodPut, "/xrpc/app.bsky.feed.getTimeline", nil, nil)
	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("status = %d, want 404 for a verb XRPC does not have", resp.StatusCode)
	}
}

func TestAnUnauthenticatedForwardIsRefused(t *testing.T) {
	f := newProxyFixture(t)
	req, _ := http.NewRequest(http.MethodGet, f.http.URL+"/xrpc/app.bsky.feed.getTimeline", nil)
	resp, err := f.http.Client().Do(req) // no Authorization at all
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("status = %d, want 401", resp.StatusCode)
	}
	if f.upstream.count() != 0 {
		t.Fatal("an unauthenticated call reached the upstream")
	}
}

func TestADenyVerdictStopsTheForward(t *testing.T) {
	f := newProxyFixture(t)
	f.authz.decide = func(AuthzInput) AuthzVerdict {
		return AuthzVerdict{XrpcError: "InvalidToken", Message: "not covered"}
	}
	resp := f.do(t, http.MethodGet, "/xrpc/app.bsky.feed.getTimeline", nil, nil)
	if resp.StatusCode != http.StatusForbidden {
		t.Fatalf("status = %d, want the verdict enforced", resp.StatusCode)
	}
	if f.upstream.count() != 0 {
		t.Fatal("a denied call reached the upstream")
	}
}

func TestAMalformedServiceRefIsInvalidRequest(t *testing.T) {
	f := newProxyFixture(t)
	for _, ref := range []string{"garbage", "did:web:x.test", "#frag", "did:web:x.test#a#b"} {
		resp := f.do(t, http.MethodGet, "/xrpc/app.bsky.feed.getTimeline",
			map[string]string{"atproto-proxy": ref}, nil)
		if resp.StatusCode != http.StatusBadRequest {
			t.Errorf("ref %q: status = %d, want 400", ref, resp.StatusCode)
		}
	}
	if f.upstream.count() != 0 {
		t.Fatal("a malformed ref reached the upstream")
	}
}

func TestAResolutionFailureIsInvalidRequest(t *testing.T) {
	f := newProxyFixture(t)
	f.resolver.err = errors.New("no such service")
	resp := f.do(t, http.MethodGet, "/xrpc/app.bsky.feed.getTimeline", nil, nil)
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("status = %d, want 400", resp.StatusCode)
	}
}

func TestAGuardDenyIsInvalidRequestAndAnUpstreamErrorIsBadGateway(t *testing.T) {
	signer, _ := testSigner(t)
	build := func(fetch StreamFetcher) *proxyFixture {
		authz := &stubAuthorizer{}
		resolver := &stubResolver{endpoint: "https://svc.test"}
		srv := NewServer(nil, nil, authz, &stubSigners{signer: signer}, nil)
		srv.flags.set(make([]byte, 32), true)
		srv.EnableProxy(ProxyConfig{AppViewDID: testAppViewDID, Resolver: resolver, Fetch: fetch})
		caller := &xrpc.Caller{ActorID: make([]byte, 32), DID: "did:fauna:alice",
			Scope: ScopeAppPass, Plane: PlaneAppCredential}
		x := xrpc.NewServer(staticCaller{caller}, nil, srv.AuthzHook, nil, nil)
		x.SetFallback(srv.ProxyFallback)
		ts := httptest.NewServer(x)
		t.Cleanup(ts.Close)
		return &proxyFixture{srv: srv, authz: authz, resolver: resolver, http: ts}
	}

	deny := build(errFetcher{err: &safefetch.DenyError{Reason: "target address is not globally routable", Host: "svc.test"}})
	if s := deny.do(t, http.MethodGet, "/xrpc/app.bsky.feed.getTimeline", nil, nil).StatusCode; s != http.StatusBadRequest {
		t.Errorf("guard deny: status = %d, want 400", s)
	}

	fail := build(errFetcher{err: errors.New("connection refused")})
	if s := fail.do(t, http.MethodGet, "/xrpc/app.bsky.feed.getTimeline", nil, nil).StatusCode; s != http.StatusBadGateway {
		t.Errorf("upstream failure: status = %d, want 502", s)
	}
}

func TestAPostBodyAndContentTypeAreForwarded(t *testing.T) {
	f := newProxyFixture(t)
	resp := f.do(t, http.MethodPost, "/xrpc/app.bsky.notification.updateSeen",
		map[string]string{"Content-Type": "application/json"},
		strings.NewReader(`{"seenAt":"2026-07-22T00:00:00Z"}`))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status = %d", resp.StatusCode)
	}
	up, body := f.upstream.request()
	if up.Method != http.MethodPost {
		t.Errorf("upstream method = %q", up.Method)
	}
	if up.Header.Get("Content-Type") != "application/json" {
		t.Errorf("Content-Type = %q, want forwarded", up.Header.Get("Content-Type"))
	}
	if !strings.Contains(string(body), "seenAt") {
		t.Errorf("upstream body = %q, want the caller's relayed", body)
	}
	_, claims, _, _ := decodeJWT(t, strings.TrimPrefix(up.Header.Get("Authorization"), "Bearer "))
	if claims["lxm"] != "app.bsky.notification.updateSeen" {
		t.Errorf("lxm = %v", claims["lxm"])
	}
}

// With the proxy NOT enabled the fallback matches nothing and the headerless
// default does not exist — the exact pre-F3-phase-3 behavior every older test
// pins.
func TestWithoutEnableProxyTheFallbackMatchesNothing(t *testing.T) {
	srv := NewServer(nil, nil, &stubAuthorizer{}, nil, nil)
	if _, ok := srv.ProxyFallback("app.bsky.feed.getTimeline", httptest.NewRequest(http.MethodGet, "/xrpc/app.bsky.feed.getTimeline", nil)); ok {
		t.Fatal("fallback matched with no proxy wired")
	}
}

// TestATruncatedUpstreamBodyTearsDownTheProxyResponse.
//
// The forward relays the upstream body straight through, so the status code is
// spent on the first byte. If the UPSTREAM then dies mid-body, the downstream
// connection is still perfectly healthy — and simply returning would close a
// well-formed chunked body, handing the client a short reply that reads as a
// complete one. (The upstream declares no Content-Length here, which is what
// makes the hazard reachable: with one forwarded, Go could not complete the
// response anyway.)
//
// Mutation barrier: drop the abort and the ReadAll below succeeds with a partial
// body, turning this red.
func TestATruncatedUpstreamBodyTearsDownTheProxyResponse(t *testing.T) {
	// A chunked upstream that sends a little and then dies.
	up := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"feed":[{"post":"parti`))
		w.(http.Flusher).Flush()
		panic(http.ErrAbortHandler)
	}))
	defer up.Close()

	signer, _ := testSigner(t)
	srv := NewServer(nil, nil, &stubAuthorizer{}, &stubSigners{signer: signer}, nil)
	srv.flags.set(make([]byte, 32), true)
	srv.EnableProxy(ProxyConfig{
		AppViewDID: testAppViewDID,
		Resolver:   &stubResolver{endpoint: up.URL},
		Fetch:      plainFetcher{},
	})
	x := xrpc.NewServer(staticCaller{&xrpc.Caller{
		ActorID: make([]byte, 32), DID: "did:fauna:alice", Handle: "alice",
		Scope: ScopeAppPass, Plane: PlaneAppCredential,
	}}, nil, srv.AuthzHook, nil, nil)
	x.SetFallback(srv.ProxyFallback)
	ts := httptest.NewServer(x)
	defer ts.Close()

	req, err := http.NewRequest(http.MethodGet, ts.URL+"/xrpc/app.bsky.feed.getTimeline", nil)
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Authorization", "Bearer client-access-token")
	resp, err := ts.Client().Do(req)
	if err != nil {
		// A torn-down response may surface as a transport error — same verdict.
		return
	}
	defer resp.Body.Close()
	// Two shapes of the same failure, and the second is the one a plain `return`
	// produces: the client reads the short body cleanly, and Go even synthesises a
	// Content-Length the upstream never sent, so the reply is self-consistent
	// nonsense.
	body, rerr := io.ReadAll(resp.Body)
	if rerr == nil {
		t.Errorf("proxy served a COMPLETE %d-byte body (%q, Content-Length %q) after the upstream truncated — a partial reply must not read as a whole one",
			len(body), body, resp.Header.Get("Content-Length"))
	}
}
