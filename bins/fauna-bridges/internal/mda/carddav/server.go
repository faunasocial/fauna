package carddav

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"sync/atomic"
	"time"

	"github.com/emersion/go-webdav/carddav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/authlock"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/dav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// carddavRealm is the WWW-Authenticate realm this CardDAV terminator advertises
// on 401 responses (RFC 7617 requires one; "fauna-carddav" scopes the
// credentials to this service). It is threaded into davauth.NewMiddleware and
// reused by the sync-collection interceptor's defensive no-session 401 so every
// CardDAV challenge is byte-identical. The CalDAV terminator passes
// "fauna-caldav" to the same davauth machinery.
const carddavRealm = "fauna-carddav"

// readHeaderTimeout is the per-request header-read deadline. CardDAV PROPFIND
// headers from well-behaved MUAs land in well under a second; a stuck client
// should not be able to occupy a server goroutine indefinitely.
const readHeaderTimeout = 30 * time.Second

// readTimeout / writeTimeout / idleTimeout time-bound the *whole* request, not
// just the header. Without them a slow-body PUT/REPORT holds a goroutine +
// buffer (the body is size-bounded at maxRequestBytes = 16 MiB, but was not
// *time*-bounded), and an idle keep-alive connection sits open indefinitely.
// Mirrors the CalDAV terminator's posture.
const (
	readTimeout  = 5 * time.Minute
	writeTimeout = 5 * time.Minute
	idleTimeout  = 2 * time.Minute
)

// Server wraps emersion/go-webdav/carddav.Handler with the Fauna-specific
// Backend + auth middleware + W1 (account-data-plane.md § Workstreams) mitigation middleware. One instance per
// listening address. SEAL-ALWAYS — no storage-mode plumbing.
//
// Slice 2c ships the servable, unit-tested package with a standalone
// Serve/Shutdown surface. Slice 2d mounts Handler() under the shared CalDAV 443
// listener via a `/caldav/` vs `/carddav/` path mux (no separate port).
type Server struct {
	inner   *http.Server
	handler http.Handler
	logger  *slog.Logger
	// lockout is the per-(credential, source-IP) AUTH-failure brake shared with
	// the auth middleware. ApplyConfig hot-swaps the pointed-to instance on a
	// `config_changed` re-fetch; the middleware Loads it per request.
	lockout *atomic.Pointer[authlock.Lockout]
	// primaryDomain is the box's PrimaryDomain, shared with the auth middleware
	// so a bare Basic-auth username (no `@domain`) resolves under it.
	// ApplyConfig hot-swaps it on a `config_changed` re-fetch; the middleware
	// Loads it per request.
	primaryDomain *atomic.Pointer[string]
}

// ServerConfig collects the per-server settings the MDA wires when constructing
// a CardDAV listener. SEAL-ALWAYS — there is no StorageMode / MailEnabled /
// LocalDomains / ClassifyTransport knob (those are CalDAV auto-schedule /
// plaintext-mode concerns with no CardDAV analog).
type ServerConfig struct {
	// TLSConfig is the live TLS config (typically wired from
	// `internal/tls.Provider.GetCertificate`). Implicit-TLS only.
	TLSConfig *tls.Config
	// Logger is the per-server structured logger.
	Logger *slog.Logger
	// MaxAuthFailuresPerMinute seeds the per-(credential, source-IP) AUTH-failure
	// lockout from `Snapshot.Auth.MaxAuthFailuresPerMinute` (catalog default 30;
	// `0` disables — the value tests leave unset). ApplyConfig rebuilds it on a
	// `config_changed` re-fetch.
	MaxAuthFailuresPerMinute uint32
	// PrimaryDomain seeds the box's primary domain (ConfigSnapshot.PrimaryDomain)
	// — the domain a bare Basic-auth username (no `@domain`) resolves under, so a
	// MUA that sends only the local part authenticates. ApplyConfig hot-swaps it
	// on a `config_changed` re-fetch. Empty ⇒ strict `user@domain` only (the
	// value tests leave unset).
	PrimaryDomain string
}

// NewServer constructs a CardDAV server bound to `client` (the MDA-process
// WS-RPC caller) and the configured TLS surface. The returned Server is not
// started; the caller invokes `Serve(ln)` with the TLS listener (standalone
// use) or mounts `Handler()` under a shared mux (Slice 2d).
//
// HTTPS-only. The auth middleware wraps the carddav.Handler so every request
// lands with a validated session in context (or 401 on the wire).
func NewServer(cfg ServerConfig, client wsrpc.Caller) *Server {
	if client == nil {
		panic("carddav: NewServer: client must not be nil")
	}
	logger := cfg.Logger
	if logger == nil {
		logger = slog.Default()
	}

	backend := NewBackend(logger)
	handler := &carddav.Handler{Backend: backend}

	// Shared AUTH-failure lockout, hot-swappable via ApplyConfig. The middleware
	// Loads it per request; the Server keeps the same pointer so a
	// config_changed rebuild swaps the ceiling without a restart.
	lockout := &atomic.Pointer[authlock.Lockout]{}
	lockout.Store(authlock.New(cfg.MaxAuthFailuresPerMinute, time.Minute, nil))

	// Shared primary domain, hot-swappable via ApplyConfig. Lets a bare
	// Basic-auth username resolve under it.
	primaryDomain := &atomic.Pointer[string]{}
	pd := cfg.PrimaryDomain
	primaryDomain.Store(&pd)

	// Middleware chain (outermost → innermost):
	//   accessLogMiddleware — one structured line per request.
	//   w1Mitigations       — body cap / XML depth / PROPFIND depth gates.
	//   davauth.NewMiddleware — HTTP Basic → AEAD-unwrap → Session in ctx.
	//   ifMatchMiddleware   — stash If-Match header for the DELETE handler.
	//   newPropPatchInterceptor — handle PROPPATCH (emersion 501s it — see
	//                       props.go): collection path → unseal/mutate/re-seal/
	//                       provision_addressbook(update_metadata=true); card
	//                       resource path → 403. Twin of the CalDAV interceptor.
	//   newSyncCollectionInterceptor — peek REPORT body; route
	//                       {DAV:}sync-collection to nest's
	//                       sync_addressbook_since RPC; pass everything else
	//                       (addressbook-query / -multiget) through to emersion.
	//   carddav.Handler     — emersion's PROPFIND/PUT/DELETE/REPORT/MKCOL handler.
	//
	// There is NO MKCALENDAR / scheduling interceptor (CalDAV-only) and NO
	// plaintext-storage plumbing (seal-always). PUT continues to receive If-Match
	// through `opts.IfMatch` per emersion's existing plumbing.
	chain := accessLogMiddleware(
		w1Mitigations(
			davauth.NewMiddleware(
				carddavRealm,
				reportBodyMiddleware(ifMatchMiddleware(
					newPropPatchInterceptor(
						newSyncCollectionInterceptor(dav.QuotaBody(handler), logger),
						logger,
					),
				)),
				client, logger, lockout, primaryDomain,
			),
		),
		logger,
	)

	// Mux mounts the chain at the catch-all path. emersion/go-webdav's
	// carddav.Handler serves the `/.well-known/carddav` redirect itself once the
	// request reaches it (after passing auth).
	mux := http.NewServeMux()
	mux.Handle("/", chain)

	inner := &http.Server{
		Handler:           mux,
		ReadHeaderTimeout: readHeaderTimeout,
		ReadTimeout:       readTimeout,
		WriteTimeout:      writeTimeout,
		IdleTimeout:       idleTimeout,
		TLSConfig:         cfg.TLSConfig,
		ErrorLog:          nil,
	}
	return &Server{
		inner:         inner,
		handler:       chain,
		logger:        logger,
		lockout:       lockout,
		primaryDomain: primaryDomain,
	}
}

// Handler returns the full CardDAV middleware+backend chain (auth → w1 →
// ifMatch → propPatch → sync-collection → emersion carddav.Handler) as a single
// http.Handler. Slice
// 2d's shared-443 mux mounts this under `/carddav/` alongside the CalDAV chain
// under `/caldav/` on ONE 443 listener, so the two DAV protocols share a TLS
// termination and port (their SRV records both target 443). For standalone use
// call Serve instead — it wraps this handler in the Server's own http.Server.
func (s *Server) Handler() http.Handler {
	return s.handler
}

// ApplyConfig hot-swaps the AUTH-failure ceiling + primary domain from a freshly
// fetched snapshot, so a `fauna.bridges.config_changed` re-fetch (or a reconnect
// re-fetch) takes effect on the next request without a restart. Registered with
// the wsrpc.ConfigReloader in mda.Run. A new lockout resets the in-flight
// counters (mirrors the CalDAV / IMAP backends); acceptable on a rare admin
// config edit. Concurrency-safe: each swap is a single atomic store the
// middleware Loads per request.
func (s *Server) ApplyConfig(snap wsrpc.ConfigSnapshot) {
	s.lockout.Store(authlock.New(snap.Auth.MaxAuthFailuresPerMinute, time.Minute, nil))
	pd := snap.PrimaryDomain
	s.primaryDomain.Store(&pd)
	s.logger.Info("carddav server hot-applied config",
		"max_auth_failures_per_minute", snap.Auth.MaxAuthFailuresPerMinute,
		"primary_domain", snap.PrimaryDomain,
	)
}

// Serve accepts connections on `ln` and processes them until `ln` is closed or
// Close is called. Returns http.ErrServerClosed as nil on a clean shutdown.
func (s *Server) Serve(ln net.Listener) error {
	err := s.inner.Serve(ln)
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}

// Close stops accepting new connections and waits up to 5s for in-flight
// requests to finish. The auth middleware's deferred `sess.Close()` zeroizes any
// active capabilities on the way out.
func (s *Server) Close() error {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := s.inner.Shutdown(ctx); err != nil {
		return fmt.Errorf("carddav: shutdown: %w", err)
	}
	return nil
}

// Shutdown gracefully drains in-flight CardDAV requests within ctx. Stdlib
// gives the graceful drain for free: http.Server.Shutdown closes the listener,
// lets active requests finish, and returns ctx.Err() if the deadline hits
// first. On a deadline expiry we force-close the stragglers (http.Server.Close)
// and report forced=true.
func (s *Server) Shutdown(ctx context.Context) (forced bool) {
	err := s.inner.Shutdown(ctx)
	if err == nil {
		return false
	}
	_ = s.inner.Close()
	return true
}
