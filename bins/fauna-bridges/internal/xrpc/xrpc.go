// Package xrpc is the ATProto XRPC server frame for the fauna-atproto-bridge
// (docs/goal/behavior/atproto-pds-full.md § Wire & process topology): a route
// table with DECLARED auth classes and a fixed middleware chain —
// source-IP rate limit → auth-class check (token verify) → authorization
// slot → handler. This is the shared interlock artifact between the F-chain
// (auth surface) and the mirror chain (S3's public read surface): there is
// exactly ONE route table; whichever chain adds a route registers it here.
//
// The auth classes are route-table declarations (C5's slots filled):
//
//	Public       — the whole mirror read surface; no token.
//	AppSession   — an app-credential access token (F1).
//	OAuthSession — an OAuth DPoP-bound token (F4; verifier lands then).
//	Session      — either authenticated plane.
//
// The authorization slot holds F3's shared-Rust D8 module (since 2026-07-22):
// every authenticated call's verdict comes from
// `fauna_bridge_atproto::authz::authorize` over the tracked Go binding, and
// the per-account external-apps kill-switch is one of its inputs rather than a
// check of its own. See internal/atprotopds/authz.go for the seat.
package xrpc

import (
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net"
	"net/http"
	"strings"
	"time"
)

// dpopNonceHeader is where a client reads the nonce it must echo (RFC 9449
// §8). Spelled here as well as in the OAuth package because the frame is what
// writes it on the resource-server plane — `http.Header.Set` canonicalizes,
// so the two spellings cannot diverge on the wire even though neither package
// imports the other's constant.
const dpopNonceHeader = "DPoP-Nonce"

// AuthClass declares which plane a route admits.
type AuthClass int

const (
	Public AuthClass = iota
	AppSession
	OAuthSession
	Session // either authenticated plane
)

// EndpointClass buckets routes for the per-IP rate-limit windows
// (§ Wire & process topology: per-IP × per-endpoint-class sliding windows;
// constants hard-coded, F1/F2 tune before ship).
type EndpointClass int

const (
	// ClassAuth: credential-guessing surface (createSession, token
	// endpoints) — the tightest window.
	ClassAuth EndpointClass = iota
	// ClassPublicRead: the anonymous mirror read surface.
	ClassPublicRead
	// ClassAuthed: authenticated non-write calls (getSession, proxying).
	ClassAuthed
	// ClassWrite: authenticated repo writes (com.atproto.repo.createRecord
	// and friends). Its own bucket rather than ClassAuthed's because a write
	// costs a nest round-trip plus a signed funnel commit, so it earns a
	// tighter window than authenticated reads — and because ClassAuthed's
	// contract says "non-write" (§ Wire & process topology lists authed
	// writes as their own starting constant).
	ClassWrite
	// ClassBlob: authenticated blob uploads (com.atproto.repo.uploadBlob).
	// Its own bucket rather than ClassWrite's for two reasons (F2.4 slice 1). A
	// record write's cost is a funnel commit plus a nest round-trip over a few
	// KB, while a blob body runs to the per-blob ceiling, so sharing ClassWrite's
	// window would let one IP push ceiling×window bytes a minute — a
	// bandwidth-and-storage cost a record write cannot express. And an image
	// post is uploadBlob *then* createRecord, so one shared bucket would
	// silently halve the effective post rate for exactly the callers who upload
	// media.
	ClassBlob
)

// String is the class's stable cross-language name, exactly as the shared-Rust
// D8 module knows it (fauna_bridge_atproto::authz::ENDPOINT_CLASS_*). An
// unknown value deliberately stringifies to "" — the module's closed world
// denies on it rather than guessing, so a class added here without teaching
// D8 about it fails closed instead of silently authorizing.
func (c EndpointClass) String() string {
	switch c {
	case ClassAuth:
		return "auth"
	case ClassPublicRead:
		return "public_read"
	case ClassAuthed:
		return "authed"
	case ClassWrite:
		return "write"
	case ClassBlob:
		return "blob"
	default:
		return ""
	}
}

// Caller is the authenticated principal a verified token resolves to.
type Caller struct {
	// ActorID is the Fauna account (32-byte pubkey).
	ActorID []byte
	// DID as carried in the token's sub claim.
	DID string
	// Handle the session was created with.
	Handle string
	// Scope of the presented token (e.g. com.atproto.appPass).
	Scope string
	// SessionID is the session-family id (refresh jti lineage).
	SessionID []byte
	// Plane that authenticated this call: "app_credential" or "oauth".
	Plane string
}

// TokenVerifier verifies a presented access token for a plane. Any failure
// must be reported as a generic error — the frame maps every verify failure
// to one uniform AuthenticationRequired with no detail passthrough, with the
// single named exception [ErrUseDPoPNonce].
type TokenVerifier interface {
	// VerifyAccess parses + verifies an access token and resolves the caller.
	//
	// The request is passed because verification can be per-REQUEST and not
	// only per-token: an OAuth access token is DPoP-bound (RFC 9449), so
	// verifying one means checking a proof over this method and URL carrying
	// this token's own hash. The app plane's bearer tokens ignore it.
	VerifyAccess(r *http.Request, token string) (*Caller, error)
}

// NonceIssuer is implemented by a [TokenVerifier] whose plane requires the
// caller to echo a server-issued nonce.
//
// **A plane that REQUIRES a nonce must ISSUE one on every response it makes,
// and that pairing is what this interface exists to make structural.** A nonce
// lives one freshness window; an access token lives far longer; so a plane that
// only ever hands nonces out somewhere *else* strands every client whose
// harvested nonce ages out mid-session — which is precisely the defect this
// interface was added to close (
// the resource-server plane required a nonce that no XRPC response ever
// carried, so every OAuth-plane call failed four minutes after the last
// authorization-server round trip, self-healing only via a token rotation and
// therefore reading as flakiness). The three authorization-server endpoints had
// kept this discipline since slice 4; the resource-server plane inherited the
// requirement without the duty.
//
// The frame calls this for every request that presents the plane's scheme —
// success and refusal alike, and before the handler runs, so the header is
// already on the ResponseWriter whatever the outcome. Issuing on refusals is
// not generosity: it is the only channel by which a client holding a stale
// nonce learns the fresh one, and issuing uniformly is what keeps the presence
// of the header from itself distinguishing one refusal from another.
type NonceIssuer interface {
	// IssueNonce returns a fresh nonce, or "" when this plane is not
	// configured on this process (the frame then sets no header).
	IssueNonce() string
}

// AuthzHook is the authorization slot run after authentication, before the
// handler — D8's seat. Returning a non-nil *Error refuses the call. The hook
// is expected to be pure assembly + enforcement around the shared-Rust module;
// authorization logic living here would be the second decision point D8 exists
// to prevent.
type AuthzHook func(r *http.Request, route *Route, caller *Caller) *Error

// Handler is an XRPC method implementation. caller is nil for Public routes.
type Handler func(w http.ResponseWriter, r *http.Request, caller *Caller)

// Route is one XRPC method registration.
type Route struct {
	// NSID, e.g. "com.atproto.server.createSession".
	NSID string
	// Method is http.MethodGet (query) or http.MethodPost (procedure).
	Method string
	// Auth is the declared auth class.
	Auth AuthClass
	// Class buckets the route for per-IP rate limiting.
	Class EndpointClass
	// Proxyable marks a route a client may redirect to another service with
	// the `atproto-proxy` header. ONLY these routes hand D8 an `aud`: a
	// forged header on any other route must not skew the decision, so it is
	// ignored rather than trusted.
	Proxyable bool
	// LongLived marks a route whose response is an open-ended STREAM rather
	// than one reply — today only the subscribeRepos firehose, which upgrades
	// to a WebSocket inside its handler.
	//
	// The frame does not arm its response-stall deadline on such a route, and
	// the route MUST bound its own writes instead (the firehose's per-frame
	// write timeout plus its ping/pong eviction). Declaring it here rather than
	// inferring it keeps the default safe: a new route is bounded unless its
	// author says it is a stream, and the reason a stream opts out — a deadline
	// would evict exactly the healthy, quiet relay we want to keep — is
	// recorded at the route rather than rediscovered as an outage.
	LongLived bool
	// Handle is the implementation.
	Handle Handler
}

// responseStallTimeout bounds how long ONE response write may stall before the
// connection is torn down. It is a stall bound, not a request budget, BY
// CONSTRUCTION: the frame hands every non-LongLived handler a
// [stallBoundWriter], which chunks each write and re-arms the deadline per
// chunk — so a peer that keeps reading is never cut off however long the body
// is, while a peer that stops reading stops holding server resources. No
// handler has to opt in for that to hold (regression was
// exactly an opt-in model: routes that did not wrap — getBlob at up to 50 MiB,
// anonymous — served under a total transfer budget in disguise).
//
// Why a stall bound is the only honest shape here: sync.getRepo's payload is the
// caller's whole live repo with no page ceiling the lexicon permits, so any
// fixed total budget would have to be generous enough for the largest legitimate
// repo over the slowest legitimate link — which bounds nothing. 30s matches the
// firehose's own per-frame write timeout (internal/atprotofirehose), for the same
// reason recorded there: availability hardening must not itself become an
// availability bug.
const responseStallTimeout = 30 * time.Second

// stallWriteChunkBytes caps how many bytes one connection-level write may
// carry under a single arming of the stall deadline. The frame's writer
// ([stallBoundWriter]) splits larger bodies and re-arms per chunk, which is
// what makes [responseStallTimeout] a bound on STALLING rather than on total
// transfer even for a handler that hands its whole payload to one Write —
// sync.getBlob's runs to the 50 MiB video ceiling, which at a single 30 s
// window would demand ~14 Mbit/s sustained from every legitimate consumer.
// At 256 KiB per window the survival floor is ~8.7 KiB/s: below that a peer
// is indistinguishable from one that stopped reading, which is exactly who
// the deadline exists to evict.
const stallWriteChunkBytes = 256 << 10

// SetResponseStallDeadline arms (or re-arms) [responseStallTimeout] on w. The
// frame calls it before and after every non-LongLived handler, and the
// [stallBoundWriter] it hands that handler calls it per chunk so the bound
// tracks progress rather than total size. One owner for "how long may a
// response stall".
//
// A ResponseWriter that cannot carry a deadline (httptest's recorder, any
// wrapper that does not unwrap) reports ErrNotSupported, which is deliberately
// ignored: the deadline is hardening on the real serving path, never a
// correctness precondition a test has to satisfy.
func SetResponseStallDeadline(w http.ResponseWriter) {
	_ = http.NewResponseController(w).SetWriteDeadline(time.Now().Add(responseStallTimeout))
}

// stallBoundWriter is the http.ResponseWriter the frame hands every
// non-LongLived handler. Its one job is making the response-stall deadline a
// stall bound BY CONSTRUCTION: every write is split into chunks of at most
// [stallWriteChunkBytes], each written under a freshly armed deadline, so one
// 30 s window only ever covers one chunk's progress no matter how the handler
// writes — one giant Write included, which is how getBlob serves and how the
// regression got in.
//
// Deliberately NOT re-exposed: http.Flusher / http.Hijacker (the CalDAV
// middleware precedent) — http.ResponseController reaches both, and the write
// deadline, through Unwrap. Handlers that need to know whether the status code
// is spent still wrap this in a [ProgressWriter]; its own re-arm is then
// redundant but harmless.
type stallBoundWriter struct {
	http.ResponseWriter
}

func (s *stallBoundWriter) Write(b []byte) (int, error) {
	if len(b) <= stallWriteChunkBytes {
		SetResponseStallDeadline(s.ResponseWriter)
		return s.ResponseWriter.Write(b)
	}
	total := 0
	for len(b) > 0 {
		chunk := b
		if len(chunk) > stallWriteChunkBytes {
			chunk = b[:stallWriteChunkBytes]
		}
		SetResponseStallDeadline(s.ResponseWriter)
		n, err := s.ResponseWriter.Write(chunk)
		total += n
		b = b[n:]
		if err != nil {
			return total, err
		}
		if n < len(chunk) {
			// io.Writer forbids a short write with a nil error; refuse to
			// spin on a writer that breaks the contract.
			return total, io.ErrShortWrite
		}
	}
	return total, nil
}

// Unwrap lets http.ResponseController reach the underlying connection's
// deadline/flush/hijack controls through this wrapper.
func (s *stallBoundWriter) Unwrap() http.ResponseWriter { return s.ResponseWriter }

// ProgressWriter counts the bytes a streaming handler has handed to the
// ResponseWriter. The byte count is its load-bearing half: it tells a failing
// handler whether the status code is still available — once bytes are out, the
// only honest answer to a mid-body failure is to tear the response down
// (http.ErrAbortHandler), because returning normally closes a well-formed
// chunked body and hands the consumer a short payload that reads as a complete
// one.
//
// It also re-arms the stall deadline before each write. Since the frame's
// [stallBoundWriter] re-arms per chunk underneath every non-LongLived handler,
// that half is defence in depth rather than load-bearing — it keeps a
// ProgressWriter used over a writer the frame did NOT wrap (a test, a future
// non-frame call site) from silently serving under a total transfer budget.
type ProgressWriter struct {
	w       http.ResponseWriter
	written int64
}

// NewProgressWriter wraps w. It writes nothing itself, so a caller that ends up
// failing before the first Write can still reply with a normal error.
func NewProgressWriter(w http.ResponseWriter) *ProgressWriter {
	return &ProgressWriter{w: w}
}

func (p *ProgressWriter) Write(b []byte) (int, error) {
	SetResponseStallDeadline(p.w)
	n, err := p.w.Write(b)
	p.written += int64(n)
	return n, err
}

// Written reports the bytes handed to the ResponseWriter so far. Zero means the
// status code is still unspent.
func (p *ProgressWriter) Written() int64 { return p.written }

// Error is the XRPC wire error shape ({"error": ..., "message": ...}).
type Error struct {
	Status  int    `json:"-"`
	Name    string `json:"error"`
	Message string `json:"message"`
}

// Uniform failures (no user-enumeration signal: a wrong secret, an unknown
// identifier, a disabled account, and a malformed token all read identically
// from outside).
func AuthRequired() *Error {
	return &Error{Status: http.StatusUnauthorized, Name: "AuthenticationRequired", Message: "authentication required"}
}

func RateLimited() *Error {
	return &Error{Status: http.StatusTooManyRequests, Name: "RateLimitExceeded", Message: "rate limit exceeded"}
}

func InvalidRequest(msg string) *Error {
	return &Error{Status: http.StatusBadRequest, Name: "InvalidRequest", Message: msg}
}

func MethodNotImplemented() *Error {
	return &Error{Status: http.StatusNotFound, Name: "MethodNotImplemented", Message: "method not implemented"}
}

// InvalidSwap is the lexicon's compare-and-swap failure: the caller pinned the
// repo commit (`swapCommit`) or a record (`swapRecord`) to a value that is no
// longer current, so the write was refused and nothing was applied.
//
// The NAME is the lexicon's own — `com.atproto.repo.{createRecord,putRecord,
// deleteRecord,applyWrites}` all declare `InvalidSwap`, and third-party clients
// match on that string to decide whether to re-read and retry. It is spelled
// without an `Error` suffix for the same reason `InvalidRequest` is.
func InvalidSwap(msg string) *Error {
	return &Error{Status: http.StatusBadRequest, Name: "InvalidSwap", Message: msg}
}

func InternalError() *Error {
	return &Error{Status: http.StatusInternalServerError, Name: "InternalServerError", Message: "internal error"}
}

// DenyFromModule maps a shared-Rust D8 deny onto the wire.
//
// This is a mechanical name → status table and NOTHING else — the decision was
// already made, in Rust. The error NAME carries the D6 refusal sub-type, so the
// status follows from it; the message is passed through verbatim. An
// unrecognized name is refused as Forbidden, never passed through as success.
func DenyFromModule(name, message string) *Error {
	switch name {
	case "AuthenticationRequired":
		// Uniform with a bad token — nothing may reveal that the account
		// exists but is disabled, or that a scope was merely insufficient.
		return AuthRequired()
	case "MethodNotImplemented":
		// Deferred, not policy. Reuse the frame's helper so one error name
		// never carries two statuses.
		e := MethodNotImplemented()
		e.Message = message
		return e
	default:
		return &Error{Status: http.StatusForbidden, Name: name, Message: message}
	}
}

// WriteError emits an XRPC error reply.
func WriteError(w http.ResponseWriter, e *Error) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(e.Status)
	_ = json.NewEncoder(w).Encode(e)
}

// WriteJSON emits a 200 JSON reply.
func WriteJSON(w http.ResponseWriter, body any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(body)
}

// FallbackFunc supplies a synthetic route for an NSID with no registration —
// F3's service-proxy catch-all (the AppView surface grows methods
// continuously; a fixed route list would break official clients on every
// addition). The returned route runs the FULL middleware chain exactly like a
// registered one — rate limit, auth class, authorization slot — with NSID set
// to the requested method so D8 sees the real `lxm`. Returning false keeps
// the closed-world MethodNotImplemented.
type FallbackFunc func(nsid string, r *http.Request) (Route, bool)

// Server is the XRPC dispatcher.
type Server struct {
	routes map[string]Route // keyed by NSID
	// verifiers per plane; nil verifier = that plane is not live yet (F4:
	// OAuthSession stays nil until the provider lands — such routes refuse
	// uniformly).
	appVerifier   TokenVerifier
	oauthVerifier TokenVerifier
	authz         AuthzHook
	limiter       *IPLimiter
	fallback      FallbackFunc
	logger        *slog.Logger
}

// NewServer builds a dispatcher. authz may be nil (allow-all slot);
// oauthVerifier is nil until F4.
func NewServer(appVerifier, oauthVerifier TokenVerifier, authz AuthzHook, limiter *IPLimiter, logger *slog.Logger) *Server {
	if logger == nil {
		logger = slog.Default()
	}
	return &Server{
		routes:        make(map[string]Route),
		appVerifier:   appVerifier,
		oauthVerifier: oauthVerifier,
		authz:         authz,
		limiter:       limiter,
		logger:        logger,
	}
}

// Register adds a route. Panics on a duplicate NSID — two chains landing the
// same method is a build-time coordination bug, never a runtime condition.
func (s *Server) Register(r Route) {
	if _, dup := s.routes[r.NSID]; dup {
		panic("xrpc: duplicate route " + r.NSID)
	}
	if r.Method != http.MethodGet && r.Method != http.MethodPost {
		panic("xrpc: route " + r.NSID + " must be GET or POST")
	}
	s.routes[r.NSID] = r
}

// RegisteredRoutes is the route table, for tests that assert a property over
// EVERY route rather than over the handful a test remembered to name.
//
// It exists because a route's declared `Auth` class is a claim about which
// credential planes may reach it, and every plane-level check below it — D8's
// matrix, the scope model — is unreachable on a route whose class excludes the
// plane. That is invisible per-route and only shows up in a sweep, which is how
// `getSession` stayed `AppSession` into F4 slice 7 and made the base OAuth
// scope unusable against the one method it is defined to cover.
//
// The returned map is a copy: a caller walking it must not be able to mutate
// the live table.
func (s *Server) RegisteredRoutes() map[string]Route {
	out := make(map[string]Route, len(s.routes))
	for nsid, r := range s.routes {
		out[nsid] = r
	}
	return out
}

// SetFallback installs the unregistered-NSID fallback. Registered routes
// always win — the fallback is consulted only on a lookup miss, so a locally
// implemented method can never be shadowed into a proxy forward.
func (s *Server) SetFallback(f FallbackFunc) {
	s.fallback = f
}

// Routes lists registered NSIDs (diagnostics/tests).
func (s *Server) Routes() []string {
	out := make([]string, 0, len(s.routes))
	for nsid := range s.routes {
		out = append(out, nsid)
	}
	return out
}

// ClientIP extracts the peer IP. The listener sits behind the SNI router
// with PROXY protocol v2, which rewrites RemoteAddr to the real client —
// forwarding headers are deliberately ignored (anon-surface convention:
// rate limits key on the transport peer, never spoofable header text).
//
// Exported so every per-peer bound on this listener keys on the SAME peer
// notion — createSession's failure lockout (internal/atprotopds) included: a
// second, local extraction is exactly how one surface starts keying on the SNI
// router's address and rate-limits the whole internet as one client.
func ClientIP(r *http.Request) string {
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil {
		return r.RemoteAddr
	}
	return host
}

// ServeHTTP dispatches /xrpc/{nsid} through the middleware chain.
func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	// 0 — the DPoP plane's standing duty, before anything can write a status.
	//
	// Placed at the very top, ahead of routing, rate limiting and auth, because
	// the nonce is owed to the *credential* the request presents and not to any
	// outcome it reaches: a 404, a 429, an authz refusal and a 200 all carry
	// one. That uniformity is load-bearing twice over — a client recovers from
	// whichever response it happens to get, and the header's presence never
	// distinguishes one refusal from another (see [NonceIssuer]).
	if _, ok := dpopPresented(r); ok {
		if issuer, canIssue := s.oauthVerifier.(NonceIssuer); canIssue {
			if nonce := issuer.IssueNonce(); nonce != "" {
				w.Header().Set(dpopNonceHeader, nonce)
			}
		}
	}

	nsid, ok := strings.CutPrefix(r.URL.Path, "/xrpc/")
	if !ok || nsid == "" || strings.ContainsRune(nsid, '/') {
		WriteError(w, InvalidRequest("not an XRPC path"))
		return
	}
	route, ok := s.routes[nsid]
	if !ok && s.fallback != nil {
		route, ok = s.fallback(nsid, r)
	}
	if !ok {
		WriteError(w, MethodNotImplemented())
		return
	}
	if r.Method != route.Method {
		WriteError(w, InvalidRequest("wrong HTTP method for "+nsid))
		return
	}

	// 1 — source-IP rate limit, per endpoint class.
	ip := ClientIP(r)
	if s.limiter != nil && !s.limiter.Allow(ip, route.Class) {
		// Log the rejection with the real source_ip. This is the ONE rate-limit
		// exit that logs (fires only on a 429 — no hot-path cost), mirroring the
		// mail AUTH surfaces' `AUTH locked out` lines. Behind the SNI router the
		// source_ip is the PROXY-v2-conveyed client IP the limiter keys on; a
		// loopback value would mean the router's `--send-proxy-to` header never
		// reached the listener's proxyproto peel. tier_4's real-client-IP proof
		// (`test_atproto_pds_sni_router.py`) greps this line.
		s.logger.Warn("xrpc: source-IP rate limit exceeded", "nsid", nsid, "source_ip", ip)
		WriteError(w, RateLimited())
		return
	}

	// 2 — auth-class check.
	var caller *Caller
	if route.Auth != Public {
		c, err := s.authenticate(r, route.Auth)
		if err != nil {
			// Uniform: no verify detail crosses the wire — with the single
			// exception of the retryable nonce challenge, whose whole point is
			// that the client can act on it (RFC 9449 §7.2). The BODY stays
			// byte-identical either way; what the challenge adds is the
			// `WWW-Authenticate` header a conformant client already knows to
			// read, alongside the fresh nonce step 0 has already set.
			if errors.Is(err, ErrUseDPoPNonce) {
				w.Header().Set("WWW-Authenticate", `DPoP error="use_dpop_nonce"`)
			}
			WriteError(w, AuthRequired())
			return
		}
		caller = c
	}

	// 3 — authorization slot.
	//
	// The closed world extends to the frame itself: an authenticated route must
	// also be *authorized*, so with no hook installed there is no decision to
	// make and we refuse rather than serve on authn alone. D8's point is one
	// decision point, never zero. Public routes are unaffected — `Auth: Public`
	// in the route table IS their authorization.
	//
	// The hook itself is still consulted unconditionally when installed, so a
	// hook that wants to see public traffic keeps seeing it.
	if route.Auth != Public && s.authz == nil {
		WriteError(w, AuthRequired())
		return
	}
	if s.authz != nil {
		if e := s.authz(r, &route, caller); e != nil {
			WriteError(w, e)
			return
		}
	}

	// 4 — response-stall bound, then the handler.
	//
	// Armed here rather than per handler so it covers every route by default,
	// including the F3 proxy fallback's synthetic ones — and the writer the
	// handler is handed re-arms it per chunk (stallBoundWriter), so the bound
	// is a stall bound by construction rather than only on the routes that
	// remembered to opt in. A stream declares itself (Route.LongLived), bounds
	// its own writes, and gets the raw writer (its handler upgrades the
	// connection); everything else answers with one reply, and a peer that
	// stops reading it stops holding this goroutine, its socket and whatever
	// the handler is mid-way through producing.
	if route.LongLived {
		route.Handle(w, r, caller)
		return
	}
	// Covers time-to-first-byte for a handler that computes before writing.
	SetResponseStallDeadline(w)
	route.Handle(&stallBoundWriter{ResponseWriter: w}, r, caller)
	// The chunked-body terminator net/http writes after the handler returns
	// runs under a fresh window too, not whatever is left of the last chunk's.
	SetResponseStallDeadline(w)
}

// authenticate resolves the caller from the Authorization header.
//
// **The SCHEME selects the plane, and that is a security property, not a
// convenience.** An app-credential token is a bearer credential by design (D3
// rung 1) and arrives as `Bearer`; an OAuth access token is DPoP-bound and
// arrives as `DPoP` (RFC 9449 §7.1). Dispatching on the scheme is what makes
// "a bound token must never be accepted as a bearer token" hold by
// construction: the `Bearer` branch never reaches the OAuth verifier, so there
// is no path on which a `cnf.jkt`-carrying token is honoured without its proof.
// Trying both verifiers against both schemes — the obvious "be liberal" shape —
// would silently turn every OAuth token into a bearer credential, discarding
// the binding the entire DPoP plane exists to provide.
func (s *Server) authenticate(r *http.Request, class AuthClass) (*Caller, error) {
	header := r.Header.Get("Authorization")
	if token, ok := strings.CutPrefix(header, "Bearer "); ok && token != "" {
		if (class == AppSession || class == Session) && s.appVerifier != nil {
			if c, err := s.appVerifier.VerifyAccess(r, token); err == nil {
				return c, nil
			}
		}
		// Deliberately NOT propagating the verifier's error: the app plane has
		// no retryable refusal, and a bearer credential must never be able to
		// reach the one branch that answers with a challenge.
		return nil, errBadToken
	}
	if token, ok := dpopPresented(r); ok {
		if (class == OAuthSession || class == Session) && s.oauthVerifier != nil {
			c, err := s.oauthVerifier.VerifyAccess(r, token)
			if err == nil {
				return c, nil
			}
			// The ONE verify failure that crosses this boundary intact. Every
			// other one — a forged token, a wrong `ath`, a `cnf.jkt` mismatch,
			// an unparseable claim — collapses into errBadToken below and stays
			// indistinguishable, which is the property this frame is built on.
			if errors.Is(err, ErrUseDPoPNonce) {
				return nil, err
			}
		}
		return nil, errBadToken
	}
	return nil, errNoToken
}

// dpopPresented reports the token presented under the DPoP scheme.
//
// ⚠ **One owner of "is this request on the DPoP plane."** Both the nonce the
// frame issues ([NonceIssuer]) and the verifier the frame dispatches to are
// decided from this single answer, so the two cannot disagree about which plane
// a request is on — the same one-parser discipline the F4 design applies to a
// `client_id` URL and to the compact DPoP JWT. A second reading of the
// `Authorization` header elsewhere would be a plane classifier that can drift
// from the one that actually authenticates.
func dpopPresented(r *http.Request) (string, bool) {
	token, ok := strings.CutPrefix(r.Header.Get("Authorization"), "DPoP ")
	return token, ok && token != ""
}

var (
	errNoToken  = &verifyError{"no bearer token"}
	errBadToken = &verifyError{"token verification failed"}

	// ErrUseDPoPNonce is the one verify failure a [TokenVerifier] may
	// distinguish to the frame, and the frame to the client (RFC 9449 §7.2):
	// the proof did not carry a nonce this server issued recently, so the call
	// is **retryable** with the nonce riding this very response.
	//
	// **Telling the client this leaks nothing, and that is why it is the only
	// exception.** Nonces are public and this server hands one out on request,
	// so the challenge names a fact the caller could obtain anyway — whereas
	// distinguishing a forged token from a wrong `ath` from an unknown account
	// would be the enumeration signal [AuthRequired] exists to deny.
	//
	// It is also reachable ONLY after the presented access token has already
	// verified (see the OAuth verifier's own ordering), so the challenge can
	// never serve as an oracle about the token itself: a caller that gets one
	// is a caller already holding a credential this server signed.
	ErrUseDPoPNonce = &verifyError{"the DPoP proof carries no nonce this server issued recently"}
)

type verifyError struct{ msg string }

func (e *verifyError) Error() string { return e.msg }
