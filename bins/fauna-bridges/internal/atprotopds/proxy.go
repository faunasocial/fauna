package atprotopds

// Service proxying — the forward half of F3 (docs/goal/behavior/
// atproto-pds-full.md § F3 detail, *Service proxying*): `atproto-proxy:
// did#fragment` → resolve the target DID's service endpoint → guard → mint a
// service JWT with the caller's own repo signing key → forward, stream back.
// Headerless `app.bsky.*` calls default to the AppView (reference-PDS
// server-side convention, § Ecosystem reality); an explicit header always
// wins.
//
// The pieces this file composes were each built and proven separately:
// resolution (internal/atprotoid/service.go), the SSRF guard + pinned dial
// (internal/safefetch → shared-Rust fetch_guard), the mint
// (serviceauth.go), and D8's verdict (authz.go — the fallback route is
// Proxyable, so the module authorizes against the SAME audience this file
// dials). What is deliberately NOT here: a second dial path (every forward
// goes through safefetch — a bypass is the bug the guard exists to prevent)
// and any policy (which lxm/aud pairs may proxy is D8's call alone).

import (
	"context"
	"errors"
	"io"
	"net/http"
	"strings"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoid"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/safefetch"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// ServiceEndpointResolver resolves `did#fragment` to the endpoint URL the
// DID's document publishes (atprotoid.ServiceResolver in production; a fake
// in tests).
type ServiceEndpointResolver interface {
	ResolveEndpoint(ctx context.Context, did, fragment string) (string, error)
}

// StreamFetcher performs the guarded forward (safefetch.Fetcher in
// production; a fake in tests). The guard lives inside it — this package
// never dials.
type StreamFetcher interface {
	DoStream(ctx context.Context, req safefetch.Request) (*http.Response, error)
}

// ProxyConfig wires the proxy path. All three fields are required.
type ProxyConfig struct {
	// AppViewDID is the headerless `app.bsky.*` default audience — the
	// shared-Rust `authz::APPVIEW_SERVICE_DID`, carried across the FFI at
	// startup so the string D8 authorizes against and the string this file
	// dials can never drift (a hard-coded Go copy is the divergence that
	// would eventually dial one service while D8 authorized another).
	AppViewDID string
	Resolver   ServiceEndpointResolver
	Fetch      StreamFetcher
}

// EnableProxy turns the service-proxy path on. Not part of NewServer because
// the proxy is separable: every F1/getServiceAuth test and any deployment
// without the fallback installed behaves exactly as before.
func (s *Server) EnableProxy(cfg ProxyConfig) {
	if cfg.AppViewDID == "" || cfg.Resolver == nil || cfg.Fetch == nil {
		panic("atprotopds: EnableProxy requires AppViewDID, Resolver and Fetch")
	}
	s.proxy = &cfg
}

// isPreferencesNSID holds PDS-private state served locally from nest state
// (F3 phase 4, RegisterRoutes) — the headerless AppView default must never
// forward it to a third party. The registered routes already shadow this
// fallback (registered routes always win), so this is defence in depth: were
// the routes ever absent, the headerless default must still refuse to proxy a
// caller's own private preferences away.
func isPreferencesNSID(nsid string) bool {
	return nsid == "app.bsky.actor.getPreferences" || nsid == "app.bsky.actor.putPreferences"
}

// proxyTarget returns the service ref (`did#fragment`) this request forwards
// to, if any. ONE implementation consulted by both D8's input assembly
// (proxyAud, authz.go) and the forward itself — the audience the module
// authorizes and the audience we dial must be the same computation, not two
// that agree today.
func (s *Server) proxyTarget(r *http.Request, nsid string) (string, bool) {
	if v := strings.TrimSpace(r.Header.Get("atproto-proxy")); v != "" {
		return v, true
	}
	if s.proxy != nil && strings.HasPrefix(nsid, "app.bsky.") && !isPreferencesNSID(nsid) {
		return s.proxy.AppViewDID, true
	}
	return "", false
}

// ProxyFallback is the frame's unregistered-NSID hook (xrpc.SetFallback):
// an unknown method with a proxy target becomes an authenticated, Proxyable
// route through the FULL middleware chain — rate limit, token verify, D8 —
// with the real NSID as `lxm`. Everything else keeps the closed-world
// MethodNotImplemented.
func (s *Server) ProxyFallback(nsid string, r *http.Request) (xrpc.Route, bool) {
	if s.proxy == nil {
		return xrpc.Route{}, false
	}
	if r.Method != http.MethodGet && r.Method != http.MethodPost {
		return xrpc.Route{}, false
	}
	if _, ok := s.proxyTarget(r, nsid); !ok {
		return xrpc.Route{}, false
	}
	return xrpc.Route{
		NSID:      nsid,
		Method:    r.Method,
		Auth:      xrpc.Session,
		Class:     xrpc.ClassAuthed,
		Proxyable: true,
		Handle:    s.proxyForward,
	}, true
}

// fwdRequestHeaders is the request-header allowlist. A proxy that copied
// everything would forward the caller's PDS access token (Authorization) to a
// third party; an allowlist fails closed when clients grow new headers.
var fwdRequestHeaders = []string{
	"Content-Type",
	"Accept-Language",
	"Atproto-Accept-Labelers",
	"X-Bsky-Topics",
}

// fwdResponseHeaders is the response-header allowlist, same posture.
var fwdResponseHeaders = []string{
	"Content-Type",
	"Content-Length",
	"Atproto-Repo-Rev",
	"Atproto-Content-Labelers",
}

// proxyForward forwards one authorized request to its resolved service and
// streams the response back. D8 already ruled on this exact lxm/aud pair —
// the fallback route is Proxyable, so the frame's authz slot saw the same
// target proxyTarget returns here.
func (s *Server) proxyForward(w http.ResponseWriter, r *http.Request, caller *xrpc.Caller) {
	nsid, _ := strings.CutPrefix(r.URL.Path, "/xrpc/")
	target, ok := s.proxyTarget(r, nsid)
	if !ok || s.proxy == nil || caller == nil {
		// Unreachable: the fallback only matched because these held.
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}

	did, fragment, err := atprotoid.ParseServiceRef(target)
	if err != nil {
		xrpc.WriteError(w, xrpc.InvalidRequest("invalid atproto-proxy service reference"))
		return
	}

	// Resolution + mint share one bounded stage; the forward below carries
	// the fetcher's own timeout.
	stageCtx, cancel := context.WithTimeout(r.Context(), rpcTimeout)
	defer cancel()

	endpoint, err := s.proxy.Resolver.ResolveEndpoint(stageCtx, did, fragment)
	if err != nil {
		s.logger.Warn("proxy: service resolution failed", "nsid", nsid, "target", target, "err", err)
		xrpc.WriteError(w, xrpc.InvalidRequest("could not resolve the requested service"))
		return
	}

	if s.signers == nil {
		s.logger.Error("proxy: no repo signer source wired")
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	signer, err := s.signers.RepoSigner(stageCtx, caller.ActorID)
	if err != nil {
		s.logger.Warn("proxy: repo signing key unavailable", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	token, err := MintServiceAuth(signer, ServiceAuthRequest{
		Iss: caller.DID,
		Aud: target,
		Lxm: nsid,
		// Lifetime unset: effectiveLifetime answers with the 60 s cap, and a
		// forwarded request has no caller-requested lifetime to shorten it.
	}, time.Now())
	if err != nil {
		s.logger.Warn("proxy: service-auth mint failed", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}

	fwdURL := strings.TrimRight(endpoint, "/") + "/xrpc/" + nsid
	if q := r.URL.RawQuery; q != "" {
		fwdURL += "?" + q
	}
	hdr := http.Header{}
	for _, k := range fwdRequestHeaders {
		for _, v := range r.Header.Values(k) {
			hdr.Add(k, v)
		}
	}
	hdr.Set("Authorization", "Bearer "+token)

	var body io.Reader
	if r.Method == http.MethodPost {
		// Bound what we relay upstream with the same cap the fetcher puts on
		// what comes back.
		body = http.MaxBytesReader(w, r.Body, safefetch.DefaultMaxBytes)
	}

	resp, err := s.proxy.Fetch.DoStream(r.Context(), safefetch.Request{
		Method: r.Method,
		URL:    fwdURL,
		Header: hdr,
		Body:   body,
	})
	if err != nil {
		var deny *safefetch.DenyError
		var tooBig *http.MaxBytesError
		switch {
		case errors.As(err, &deny):
			// The refused target is the caller's own choice — the module's
			// stable reason tells them what to fix and reveals nothing.
			xrpc.WriteError(w, xrpc.InvalidRequest("proxy target refused: "+deny.Reason))
		case errors.As(err, &tooBig):
			xrpc.WriteError(w, xrpc.InvalidRequest("request body exceeds the proxy cap"))
		default:
			s.logger.Warn("proxy: upstream request failed", "nsid", nsid, "target", target, "err", err)
			xrpc.WriteError(w, &xrpc.Error{
				Status: http.StatusBadGateway, Name: "UpstreamFailure",
				Message: "upstream request failed",
			})
		}
		return
	}
	defer func() { _ = resp.Body.Close() }()

	for _, k := range fwdResponseHeaders {
		for _, v := range resp.Header.Values(k) {
			w.Header().Add(k, v)
		}
	}
	w.WriteHeader(resp.StatusCode)
	// Relayed through a ProgressWriter, so the frame's response-stall deadline
	// re-arms per chunk: an upstream body may run to safefetch's 5 MiB cap, and a
	// one-shot deadline would turn "a big reply to a slow client" into a failure.
	if _, err := io.Copy(xrpc.NewProgressWriter(w), resp.Body); err != nil {
		// The status is already on the wire; all we can do is cut the
		// connection short so the client sees a broken transfer, not a
		// complete-looking short body.
		s.logger.Warn("proxy: response stream aborted", "nsid", nsid, "err", err)
		// Returning here would NOT cut it short: the upstream-read-failed case
		// arrives with a healthy downstream connection, and Go would finish the
		// response for us — even synthesising a Content-Length — handing the
		// client a self-consistent short body. ErrAbortHandler tears it down
		// instead (same contract as sync.getRepo's streamed CAR).
		panic(http.ErrAbortHandler)
	}
}
