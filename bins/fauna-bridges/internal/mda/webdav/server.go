package webdav

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

	"github.com/emersion/go-webdav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/authlock"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// webdavRealm is the WWW-Authenticate realm this terminator advertises on 401s
// (RFC 7617). "fauna-webdav" scopes the credentials to this service, alongside
// the CalDAV ("fauna-caldav") and CardDAV ("fauna-carddav") siblings on the
// same davauth machinery.
const webdavRealm = "fauna-webdav"

const readHeaderTimeout = 30 * time.Second

// A WebDAV PUT/GET of a large file legitimately runs longer than a card/cal
// REPORT; time-bound the whole request generously (the body is size-bounded by
// maxPutBytes) and keep idle keep-alives short.
const (
	readTimeout  = 30 * time.Minute
	writeTimeout = 30 * time.Minute
	idleTimeout  = 2 * time.Minute
)

// Server wraps emersion/go-webdav's Handler with the Fauna WebDAV Backend + the
// shared davauth/w1/access-log middleware. One instance per listening address.
// Slice 4b mounts Handler() under the shared DAV 443 listener via the
// `/webdav/` mux arm (no separate port; webdav-server.md § Process topology).
type Server struct {
	inner         *http.Server
	handler       http.Handler
	logger        *slog.Logger
	lockout       *atomic.Pointer[authlock.Lockout]
	primaryDomain *atomic.Pointer[string]
}

// ServerConfig collects the per-server settings the MDA wires when constructing
// the WebDAV listener.
type ServerConfig struct {
	// TLSConfig is the live TLS config (implicit-TLS only).
	TLSConfig *tls.Config
	// Logger is the per-server structured logger.
	Logger *slog.Logger
	// MaxAuthFailuresPerMinute seeds the per-(credential, source-IP) AUTH-failure
	// lockout from Snapshot.Auth.MaxAuthFailuresPerMinute. ApplyConfig rebuilds it.
	MaxAuthFailuresPerMinute uint32
	// PrimaryDomain seeds the box's primary domain — the domain a bare Basic-auth
	// username (no @domain) resolves under. ApplyConfig hot-swaps it.
	PrimaryDomain string
	// NestBaseURL is the nest's external base URL (`https://host`, no /api/v1
	// suffix) the byte-route client hits for chunk/manifest upload/download.
	NestBaseURL string
	// NestHTTPClient is the shared HTTP client for the byte routes (reuses the
	// WS-RPC dial's loopback TLS-skip client for the in-container nest).
	NestHTTPClient *http.Client
}

// NewServer constructs a WebDAV server bound to `client` (the MDA-process WS-RPC
// caller) and the byte-route HTTP client. The returned Server is not started;
// the caller mounts Handler() under the shared DAV mux.
func NewServer(cfg ServerConfig, client wsrpc.Caller) *Server {
	if client == nil {
		panic("webdav: NewServer: client must not be nil")
	}
	logger := cfg.Logger
	if logger == nil {
		logger = slog.Default()
	}

	backend := NewBackend(logger, client, byteplane.New(cfg.NestBaseURL, cfg.NestHTTPClient))
	handler := &webdav.Handler{FileSystem: backend}

	lockout := &atomic.Pointer[authlock.Lockout]{}
	lockout.Store(authlock.New(cfg.MaxAuthFailuresPerMinute, time.Minute, nil))

	primaryDomain := &atomic.Pointer[string]{}
	pd := cfg.PrimaryDomain
	primaryDomain.Store(&pd)

	// Middleware chain (outermost → innermost):
	//   accessLogMiddleware — one structured line per request.
	//   w1Mitigations       — PROPFIND depth gate; XML-depth check; streaming PUT cap.
	//   authDispatch        — `Authorization: DPoP` → the bearer door, else Basic:
	//     davauth.NewMiddleware — HTTP Basic → AEAD-unwrap → Session in ctx;
	//       quotaMiddleware     — RFC 4331 quota-used/available-bytes on PROPFIND
	//                             (quota.go; needs the Session davauth injected).
	//     bearerMiddleware      — a principal's token admitted through the nest
	//                             (bearer.go): read methods only, its admitted
	//                             folders and keys header in ctx; no quota (the
	//                             owner's storage meter is not a principal's).
	//   webdav.Handler       — emersion's PROPFIND/GET/PUT/DELETE/MKCOL/COPY/MOVE.
	//
	// No PROPPATCH/REPORT interceptors (CalDAV/CardDAV-only — WebDAV props are
	// read-only, and there is no sync-REPORT). If-Match/If-None-Match reach the
	// FileSystem via emersion's CreateOptions/RemoveAllOptions and are enforced
	// authoritatively at the nest (webdav_record_change → conflict → 412).
	chain := accessLogMiddleware(
		w1Mitigations(
			authDispatch(
				davauth.NewMiddleware(
					webdavRealm,
					quotaMiddleware(handler, logger),
					client, logger, lockout, primaryDomain,
				),
				bearerMiddleware(handler, client, logger),
			),
		),
		logger,
	)

	mux := http.NewServeMux()
	mux.Handle("/", chain)

	inner := &http.Server{
		Handler:           mux,
		ReadHeaderTimeout: readHeaderTimeout,
		ReadTimeout:       readTimeout,
		WriteTimeout:      writeTimeout,
		IdleTimeout:       idleTimeout,
		TLSConfig:         cfg.TLSConfig,
	}
	return &Server{
		inner:         inner,
		handler:       chain,
		logger:        logger,
		lockout:       lockout,
		primaryDomain: primaryDomain,
	}
}

// Handler returns the full WebDAV middleware+backend chain as a single
// http.Handler. The shared-443 mux mounts this under `/webdav/`.
func (s *Server) Handler() http.Handler {
	return s.handler
}

// ApplyConfig hot-swaps the AUTH-failure ceiling + primary domain from a freshly
// fetched snapshot (a config_changed / reconnect re-fetch takes effect on the
// next request without a restart). Registered with the wsrpc.ConfigReloader.
func (s *Server) ApplyConfig(snap wsrpc.ConfigSnapshot) {
	s.lockout.Store(authlock.New(snap.Auth.MaxAuthFailuresPerMinute, time.Minute, nil))
	pd := snap.PrimaryDomain
	s.primaryDomain.Store(&pd)
	s.logger.Info("webdav server hot-applied config",
		"max_auth_failures_per_minute", snap.Auth.MaxAuthFailuresPerMinute,
		"primary_domain", snap.PrimaryDomain,
	)
}

// Serve accepts connections on `ln` until it is closed or Close is called.
func (s *Server) Serve(ln net.Listener) error {
	err := s.inner.Serve(ln)
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}

// Close stops accepting new connections and drains in-flight requests (≤5s).
func (s *Server) Close() error {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := s.inner.Shutdown(ctx); err != nil {
		return fmt.Errorf("webdav: shutdown: %w", err)
	}
	return nil
}

// Shutdown gracefully drains in-flight requests within ctx; force-closes
// stragglers on deadline and reports forced=true.
func (s *Server) Shutdown(ctx context.Context) (forced bool) {
	err := s.inner.Shutdown(ctx)
	if err == nil {
		return false
	}
	_ = s.inner.Close()
	return true
}
