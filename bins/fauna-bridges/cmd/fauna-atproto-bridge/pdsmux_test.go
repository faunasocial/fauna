package main

import (
	"encoding/base64"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	faunaAtproto "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_bridge_atproto"
)

// testApex is the deployment's claimed domain in these tests; the PDS host is
// derived from it by the shared-Rust owner, exactly as main.go derives it.
const testApex = "example.com"

// testPDSHost is `pds.<apex>`, read through the same export main.go uses.
var testPDSHost = faunaAtproto.AtprotoPdsHost(testApex)

// TestPDSMuxRoutesXRPCAndDIDDoc pins the listener's HTTP surface: /xrpc/ goes to
// the route table and /.well-known/did.json is a plain route beside it (a DID
// document is not an XRPC method, so serving it through the XRPC dispatcher
// would answer MethodNotImplemented).
func TestPDSMuxRoutesXRPCAndDIDDoc(t *testing.T) {
	reachedXRPC := false
	xrpcStub := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		reachedXRPC = true
		w.WriteHeader(http.StatusTeapot)
	})
	mux, err := newPDSMux(xrpcStub, testPDSHost, testApex)
	if err != nil {
		t.Fatalf("newPDSMux: %v", err)
	}
	ts := httptest.NewServer(mux)
	defer ts.Close()

	resp, err := http.Get(ts.URL + "/.well-known/did.json")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("did.json status = %d, want 200", resp.StatusCode)
	}
	if ct := resp.Header.Get("Content-Type"); ct != "application/did+ld+json" {
		t.Errorf("did.json content-type = %q, want application/did+ld+json", ct)
	}
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		ID string `json:"id"`
	}
	if err := json.Unmarshal(body, &doc); err != nil {
		t.Fatalf("did.json is not JSON: %v", err)
	}
	if doc.ID != "did:web:pds.example.com" {
		t.Errorf("did.json id = %q, want the service DID for the listener host", doc.ID)
	}
	if reachedXRPC {
		t.Error("did.json was dispatched through the XRPC route table")
	}

	xr, err := http.Get(ts.URL + "/xrpc/com.atproto.server.describeServer")
	if err != nil {
		t.Fatal(err)
	}
	defer xr.Body.Close()
	if !reachedXRPC || xr.StatusCode != http.StatusTeapot {
		t.Errorf("/xrpc/ did not reach the route table (reached=%v status=%d)", reachedXRPC, xr.StatusCode)
	}
}

// TestPDSDIDDocRejectsNonGET keeps the plain route read-only: a DID document is
// published state, never a write surface.
func TestPDSDIDDocRejectsNonGET(t *testing.T) {
	mux, err := newPDSMux(http.NotFoundHandler(), testPDSHost, testApex)
	if err != nil {
		t.Fatalf("newPDSMux: %v", err)
	}
	ts := httptest.NewServer(mux)
	defer ts.Close()

	resp, err := http.Post(ts.URL+"/.well-known/did.json", "application/json", nil)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusMethodNotAllowed {
		t.Errorf("POST did.json status = %d, want 405", resp.StatusCode)
	}
}

// retiredASPaths are the authorization server's own routes — every path the
// bridge-hosted AS once served on this host. The nest serves them now, on the
// apex; the PDS must answer none of them.
var retiredASPaths = []string{
	"/oauth/par",
	"/oauth/authorize",
	"/oauth/authorize/poll",
	"/oauth/token",
	"/oauth/revoke",
	"/oauth/jwks",
	"/.well-known/oauth-authorization-server",
}

// TestThePDSNamesTheNestAsItsAuthorizationServerAndServesNoneOfItsRoutes is the
// pairing `authorization-server.md` § The issuer owes: the re-point and the
// retirement are ONE change, and this pins both halves against the REAL mux.
//
//   - The protected-resource document names the NEST's issuer (`https://<apex>`)
//     in `authorization_servers` — not this PDS's own origin, which is where the
//     retired bridge AS lived.
//   - None of the authorization server's routes is mounted here. Leaving them up
//     while the document named the nest is what would turn a sign-out into a
//     lie: a client re-discovering through the re-pointed document presents
//     bridge-minted artifacts at the nest's /oauth/revoke, which cannot identify
//     them and answers the uniform empty 200 while doing nothing.
//
// ⚠ Asserted on the mux's routing (a 404 from a mux with a catch-all-free
// table), never on a revoke response: a revocation endpoint answers 200 to
// every token by design, so a response-shaped assertion could not tell a
// retired route from a live one.
func TestThePDSNamesTheNestAsItsAuthorizationServerAndServesNoneOfItsRoutes(t *testing.T) {
	mux, err := newPDSMux(http.NotFoundHandler(), testPDSHost, testApex)
	if err != nil {
		t.Fatalf("newPDSMux: %v", err)
	}
	ts := httptest.NewServer(mux)
	defer ts.Close()

	resp, err := http.Get(ts.URL + protectedResourcePath)
	if err != nil {
		t.Fatal(err)
	}
	body, err := io.ReadAll(resp.Body)
	resp.Body.Close()
	if err != nil {
		t.Fatal(err)
	}
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("GET %s = %d, want 200", protectedResourcePath, resp.StatusCode)
	}
	if ct := resp.Header.Get("Content-Type"); ct != "application/json" {
		t.Errorf("Content-Type = %q, want application/json", ct)
	}
	// A browser-based client reads this document from its own origin before it
	// can find the issuer (`authorization-server.md` § The issuer → *Cross-origin
	// access*): open, credential-less CORS, like the nest's issuer plane.
	if acao := resp.Header.Get("Access-Control-Allow-Origin"); acao != "*" {
		t.Errorf("Access-Control-Allow-Origin = %q, want * — a browser client on a foreign origin cannot read the document without it", acao)
	}
	var doc struct {
		Resource             string   `json:"resource"`
		AuthorizationServers []string `json:"authorization_servers"`
	}
	if uerr := json.Unmarshal(body, &doc); uerr != nil {
		t.Fatalf("the protected-resource document is not JSON: %v", uerr)
	}
	pdsOrigin := faunaAtproto.OauthIssuer(testPDSHost)
	nestIssuer := faunaAtproto.OauthIssuer(testApex)
	if doc.Resource != pdsOrigin {
		t.Errorf("resource = %q, want this PDS (%q)", doc.Resource, pdsOrigin)
	}
	if len(doc.AuthorizationServers) != 1 || doc.AuthorizationServers[0] != nestIssuer {
		t.Fatalf("authorization_servers = %v, want exactly the nest's issuer [%q] — "+
			"never this PDS's own origin, where the retired AS lived", doc.AuthorizationServers, nestIssuer)
	}

	// Every method, so a POST-only route that 405s a GET cannot pass as absent.
	for _, path := range retiredASPaths {
		for _, method := range []string{http.MethodGet, http.MethodPost} {
			req, rerr := http.NewRequest(method, ts.URL+path, http.NoBody)
			if rerr != nil {
				t.Fatal(rerr)
			}
			r, derr := http.DefaultClient.Do(req)
			if derr != nil {
				t.Fatal(derr)
			}
			r.Body.Close()
			if r.StatusCode != http.StatusNotFound {
				t.Errorf("%s %s = %d, want 404 — the authorization server's routes are the "+
					"nest's, and this PDS must mount none of them", method, path, r.StatusCode)
			}
		}
	}
}

// The mux's own table, read directly: no pattern the retired AS used resolves
// to a handler at all. The HTTP walk above proves the answer a client gets;
// this proves no route exists that happens to answer 404 itself, and it keeps
// passing only while the paths are unregistered rather than merely unreachable.
func TestNoRetiredASPatternIsRegisteredOnThePDSMux(t *testing.T) {
	mux, err := newPDSMux(http.NotFoundHandler(), testPDSHost, testApex)
	if err != nil {
		t.Fatalf("newPDSMux: %v", err)
	}
	for _, path := range retiredASPaths {
		req := httptest.NewRequest(http.MethodGet, path, nil)
		if _, pattern := mux.Handler(req); pattern != "" {
			t.Errorf("%s resolves to the registered pattern %q — the route is mounted", path, pattern)
		}
	}
	req := httptest.NewRequest(http.MethodGet, protectedResourcePath, nil)
	if _, pattern := mux.Handler(req); pattern != protectedResourcePath {
		t.Errorf("the protected-resource document resolves to %q, want its own pattern", pattern)
	}
}

// A deployment with no claimed domain has no authorization server to name: the
// issuer identifier IS the nest's apex. The document answers 503 — the same
// shape the nest's own issuer surfaces take for that state — rather than
// inventing an issuer (this host, an IP) whose tokens would stop verifying the
// moment a real domain is claimed.
func TestADomainlessPDSServesNoAuthorizationServer(t *testing.T) {
	mux, err := newPDSMux(http.NotFoundHandler(), "127.0.0.1:8447", "")
	if err != nil {
		t.Fatalf("newPDSMux: %v", err)
	}
	ts := httptest.NewServer(mux)
	defer ts.Close()

	resp, err := http.Get(ts.URL + protectedResourcePath)
	if err != nil {
		t.Fatal(err)
	}
	body, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	if resp.StatusCode != http.StatusServiceUnavailable {
		t.Fatalf("domainless GET %s = %d, want 503", protectedResourcePath, resp.StatusCode)
	}
	if strings.Contains(string(body), "authorization_servers") {
		t.Errorf("a domainless PDS served a document naming an authorization server: %s", body)
	}
	for _, path := range retiredASPaths {
		r, gerr := http.Get(ts.URL + path)
		if gerr != nil {
			t.Fatal(gerr)
		}
		r.Body.Close()
		if r.StatusCode != http.StatusNotFound {
			t.Errorf("domainless GET %s = %d, want 404", path, r.StatusCode)
		}
	}
}

// The document is published state: GET only, on either branch.
func TestTheProtectedResourceDocumentRejectsNonGET(t *testing.T) {
	for _, apex := range []string{testApex, ""} {
		mux, err := newPDSMux(http.NotFoundHandler(), testPDSHost, apex)
		if err != nil {
			t.Fatalf("newPDSMux: %v", err)
		}
		ts := httptest.NewServer(mux)
		resp, err := http.Post(ts.URL+protectedResourcePath, "application/json", nil)
		if err != nil {
			t.Fatal(err)
		}
		resp.Body.Close()
		ts.Close()
		if resp.StatusCode != http.StatusMethodNotAllowed {
			t.Errorf("apex %q: POST %s = %d, want 405", apex, protectedResourcePath, resp.StatusCode)
		}
	}
}

// TestPDSListenerBoundsHeadersAndIdleButNeverReadsOrWrites pins the listener's
// two bounds AND the two deliberate zeroes, and the zeroes are the load-bearing
// half.
//
// ReadHeaderTimeout / IdleTimeout close the phases no route can bound from
// inside: a peer dribbling request headers holds a goroutine and an fd before any
// route, rate limit or auth class has been resolved, and an idle keep-alive
// connection holds one after.
//
// ReadTimeout / WriteTimeout must stay ZERO. com.atproto.sync.subscribeRepos is a
// WebSocket on this same server, held open for hours and legitimately silent
// throughout, so a server-WIDE read or write deadline would evict exactly the
// healthy relays the firehose feeds. The per-request bound belongs one layer in,
// where it can tell a stream from a reply (internal/xrpc's response-stall
// deadline, skipped for Route.LongLived). Setting either here would be a silent
// availability regression — nothing else in the tree would notice.
func TestPDSListenerBoundsHeadersAndIdleButNeverReadsOrWrites(t *testing.T) {
	srv := newPDSServer(http.NewServeMux())

	if srv.ReadHeaderTimeout != pdsReadHeaderTimeout || srv.ReadHeaderTimeout == 0 {
		t.Errorf("ReadHeaderTimeout = %v, want %v — a slow-header peer must not hold a goroutine indefinitely on a public listener", srv.ReadHeaderTimeout, pdsReadHeaderTimeout)
	}
	if srv.IdleTimeout != pdsIdleTimeout || srv.IdleTimeout == 0 {
		t.Errorf("IdleTimeout = %v, want %v (stated, not inherited from ReadTimeout)", srv.IdleTimeout, pdsIdleTimeout)
	}
	if srv.ReadTimeout != 0 {
		t.Errorf("ReadTimeout = %v, want 0 — a server-wide read deadline kills the subscribeRepos firehose, which is silent by design", srv.ReadTimeout)
	}
	if srv.WriteTimeout != 0 {
		t.Errorf("WriteTimeout = %v, want 0 — a server-wide write deadline evicts healthy, quiet firehose relays; the per-route stall deadline is the bound", srv.WriteTimeout)
	}
}

// ── The max-size grant's token clears this listener ──────────────────────────

const (
	maxGrantAudience = "did:web:svc.example"
	// Long enough that the BYTE cap binds before the 128-scope count cap — the
	// byte cap is the one under test, and a member short enough to hit the count
	// cap first would silently retarget this test.
	maxGrantMemberPrefix = "com.example.calendar.sync.pushNotificationsBatch"
)

// maxGrantMember renders the i-th member's lxm. Letters only after the prefix:
// an NSID's name segment must begin with a letter.
func maxGrantMember(i int) string {
	letters := []byte{byte('a' + i/676%26), byte('a' + i/26%26), byte('a' + i%26)}
	return maxGrantMemberPrefix + string(letters)
}

// expandedScopesFor is the effective scope set of a grant whose permission set
// has n members: the base scope plus each member with the include's audience
// applied — the shape the shared expander produces.
func expandedScopesFor(n int) []string {
	out := make([]string, 0, n+1)
	out = append(out, "atproto")
	for i := 0; i < n; i++ {
		out = append(out, "rpc:"+maxGrantMember(i)+"?aud="+maxGrantAudience)
	}
	return out
}

// maxSizeMemberCount asks the REAL cap where the boundary is, instead of
// transcribing 8192 into this file: a Go-side copy of the constant would pass
// forever after the Rust cap changed, and "max size" is only meaningful as *the
// largest grant the issuer will mint* — a property of the shipped checker.
func maxSizeMemberCount(t *testing.T) int {
	t.Helper()
	const ceiling = 512
	for n := 1; n <= ceiling; n++ {
		if _, refused := faunaAtproto.CheckGrantExpansion(1, expandedScopesFor(n)).(faunaAtproto.GrantExpansionRefused); refused {
			if n == 1 {
				t.Fatal("even a single member is over the cap — the fixture's member names outgrew the budget they are meant to fill")
			}
			return n - 1
		}
	}
	t.Fatalf("no refusal below %d members: the caps are not bounding this fixture at all", ceiling)
	return 0
}

// TestMaxSizeGrantTokenClearsTheProductionListener is the round trip
// `atproto-pds-full.md:329` consequence 2 promises and `permission_set.rs`
// restates at the cap itself: the largest grant the issuer will mint produces an
// access token this PDS's own listener still accepts in an Authorization header.
//
// The cap is 8 KiB of expanded scope because that is the *ecosystem's* ordinary
// reverse-proxy per-header budget — not ours. Our own listener takes Go's 1 MiB
// default, so nothing in this tree would notice the cap being crossed; that
// headroom is exactly why the round trip is worth pinning rather than reasoning
// about. The nest mints the token now, but the header lands HERE, so the
// listener half of the promise is this binary's to keep.
func TestMaxSizeGrantTokenClearsTheProductionListener(t *testing.T) {
	members := maxSizeMemberCount(t)
	// The boundary is real in both directions: one more member is refused.
	if _, refused := faunaAtproto.CheckGrantExpansion(1, expandedScopesFor(members+1)).(faunaAtproto.GrantExpansionRefused); !refused {
		t.Fatalf("%d members is not the boundary — %d was still accepted", members, members+1)
	}
	scopes := expandedScopesFor(members)
	scope := strings.Join(scopes, " ")

	// A token shaped like the nest's: every claim the issuer mints, with the
	// max-size scope claim. The signature is a fixed-width ES256 r‖s; the
	// listener's bound is on bytes, not on validity.
	seg := func(v any) string {
		raw, err := json.Marshal(v)
		if err != nil {
			t.Fatal(err)
		}
		return base64.RawURLEncoding.EncodeToString(raw)
	}
	token := seg(map[string]string{"alg": "ES256", "typ": "at+jwt", "kid": strings.Repeat("k", 43)}) + "." +
		seg(map[string]any{
			"iss": faunaAtproto.OauthIssuer(testApex), "sub": "did:plc:7iza6de2dwap2sbkpav7c6c6",
			"aud": faunaAtproto.AtprotoPdsServiceDid(testApex), "scope": scope,
			"iat": 1_800_000_000, "exp": 1_800_000_900, "jti": strings.Repeat("j", 22),
			"cnf": map[string]string{"jkt": strings.Repeat("t", 43)}, "sid": strings.Repeat("s", 22),
			"client_id": "https://app.example.com/client.json", "fauna_actor": strings.Repeat("ab", 32),
		}) + "." + base64.RawURLEncoding.EncodeToString(make([]byte, 64))

	authorization := "DPoP " + token
	served, status := serveThroughProductionListener(t, authorization)
	if !served {
		t.Fatalf("the production listener REFUSED a max-size grant's token with %d: the Authorization header was "+
			"%d bytes (a %d-byte scope claim, from %d expanded scopes at the cap). newPDSServer sets no "+
			"MaxHeaderBytes, so this must clear Go's 1 MiB default with room — the expansion cap is sized "+
			"against the ECOSYSTEM's 8 KiB reverse-proxy budget rather than ours, so a refusal here means our "+
			"own listener has become the tighter bound and the cap no longer describes what this PDS will serve",
			status, len(authorization), len(scope), len(scopes))
	}

	// The control leg: the same listener DOES refuse a header past its bound, so
	// the pass above is the listener accepting something rather than this test
	// being unable to observe a refusal at all.
	//
	// 2 MiB, not the 1 MiB bound itself: Go reads up to `MaxHeaderBytes + 4096`
	// (net/http's `initialReadLimitSize`), so a header of exactly the bound is
	// still served.
	if served, status := serveThroughProductionListener(t, "DPoP "+strings.Repeat("x", 2<<20)); served {
		t.Errorf("a 2 MiB header was served (%d) — this test cannot tell an accepted header from a refused one, "+
			"so its main assertion proves nothing", status)
	}
}

// serveThroughProductionListener runs one request carrying the given
// Authorization header through a listener built by the PRODUCTION newPDSServer,
// over a real TCP connection.
//
// The production builder rather than a copy of its bounds: `MaxHeaderBytes` is
// the bound under test and newPDSServer sets it only by NOT setting it (Go's
// 1 MiB default), so a test that constructed its own http.Server would be
// asserting against its own choice and would stay green the day the production
// one changed. Over a real connection rather than httptest.NewRequest, because
// an in-process ResponseRecorder never parses a header at all — the limit is
// enforced by the server's connection reader.
func serveThroughProductionListener(t *testing.T, authorization string) (served bool, status int) {
	t.Helper()
	mux := http.NewServeMux()
	reached := make(chan string, 1)
	mux.HandleFunc("/xrpc/com.atproto.server.getSession", func(w http.ResponseWriter, r *http.Request) {
		select {
		case reached <- r.Header.Get("Authorization"):
		default:
		}
		w.WriteHeader(http.StatusOK)
	})

	srv := newPDSServer(mux)
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	go func() { _ = srv.Serve(ln) }()
	t.Cleanup(func() { _ = srv.Close() })

	req, err := http.NewRequest(http.MethodGet,
		"http://"+ln.Addr().String()+"/xrpc/com.atproto.server.getSession", http.NoBody)
	if err != nil {
		t.Fatalf("build request: %v", err)
	}
	req.Header.Set("Authorization", authorization)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		// A connection the server tore down rather than answering is a refusal
		// too — report it as one instead of failing the test here, so the
		// caller's own message explains what it means.
		return false, 0
	}
	defer resp.Body.Close()
	_, _ = io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != http.StatusOK {
		return false, resp.StatusCode
	}
	select {
	case got := <-reached:
		if got != authorization {
			t.Fatalf("the handler saw a %d-byte Authorization header, sent %d — the listener truncated it",
				len(got), len(authorization))
		}
	default:
		t.Fatal("the request answered 200 without reaching the handler")
	}
	return true, resp.StatusCode
}
