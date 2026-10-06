package dav

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"time"
)

// readHeaderTimeout is the per-request header-read deadline. DAV PROPFIND
// headers from well-behaved clients land in well under a second; a stuck client
// should not be able to occupy a server goroutine indefinitely. These are the
// PRODUCTION timeouts for the shared 443 listener; the caldav / carddav
// packages carry identical constants for their standalone (test) Serve path.
const readHeaderTimeout = 30 * time.Second

// readTimeout / writeTimeout / idleTimeout time-bound the *whole* request, not
// just the header. Without them a slow-body PUT/REPORT holds a goroutine +
// buffer (the body is size-bounded at the per-protocol maxResourceSizeBytes =
// 16 MiB, but was not *time*-bounded), and an idle keep-alive connection sits
// open indefinitely. Mirrors the CalDAV / CardDAV terminators' posture.
const (
	readTimeout  = 5 * time.Minute
	writeTimeout = 5 * time.Minute
	idleTimeout  = 2 * time.Minute
)

// Mount pairs a URL path pattern (http.ServeMux syntax) with the handler that
// serves it. The mux routes by longest-prefix match, so a `/carddav/` mount
// wins over a `/` catch-all for `/carddav/…` requests.
type Mount struct {
	// Pattern is an http.ServeMux pattern, e.g. "/" (catch-all) or "/carddav/".
	Pattern string
	// Handler is the per-protocol middleware+backend chain (from
	// caldav.Server.Handler() / carddav.Server.Handler()).
	Handler http.Handler
}

// ServerConfig collects the per-server settings the MDA wires when constructing
// the shared DAV listener.
type ServerConfig struct {
	// TLSConfig is the live TLS config (typically wired from
	// `internal/tls.Provider.GetCertificate`). Implicit-TLS only — the DAV
	// protocols upgrade implicitly over HTTPS.
	TLSConfig *tls.Config
	// Logger is the per-server structured logger.
	Logger *slog.Logger
}

// Server wraps one http.Server + http.ServeMux that terminates TLS on the
// shared 443 listener and routes each request to the mounted per-protocol DAV
// handler. One instance per listening address.
type Server struct {
	inner  *http.Server
	logger *slog.Logger
}

// NewServer builds the shared DAV server from the given mounts. Each Mount's
// pattern is registered on one http.ServeMux behind one http.Server carrying
// the shared DAV timeouts + TLS config. The returned Server is not started; the
// caller invokes Serve(ln) with the TLS listener.
//
// Registering the same pattern twice panics (stdlib ServeMux behavior), which
// is the correct fail-loud for a wiring bug; the MDA never builds a mount list
// with a duplicate pattern (see mda.davMounts).
func NewServer(cfg ServerConfig, mounts ...Mount) *Server {
	logger := cfg.Logger
	if logger == nil {
		logger = slog.Default()
	}
	mux := http.NewServeMux()
	for _, m := range mounts {
		mux.Handle(m.Pattern, m.Handler)
	}
	inner := &http.Server{
		Handler:           mux,
		ReadHeaderTimeout: readHeaderTimeout,
		ReadTimeout:       readTimeout,
		WriteTimeout:      writeTimeout,
		IdleTimeout:       idleTimeout,
		TLSConfig:         cfg.TLSConfig,
		ErrorLog:          nil, // emersion/go-webdav logs to our slog via the backends
	}
	return &Server{inner: inner, logger: logger}
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
// requests to finish. The per-protocol auth middleware's deferred
// `sess.Close()` zeroizes any active capabilities on the way out.
func (s *Server) Close() error {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := s.inner.Shutdown(ctx); err != nil {
		return fmt.Errorf("dav: shutdown: %w", err)
	}
	return nil
}

// Shutdown gracefully drains in-flight DAV requests within ctx. Stdlib gives
// the graceful drain for free: http.Server.Shutdown closes the listener, lets
// active requests finish, and returns ctx.Err() if the deadline hits first. On
// a deadline expiry we force-close the stragglers (http.Server.Close) and
// report forced=true so the MDA role can map it to
// bridgeshutdown.ErrShutdownForced. (HTTP doesn't expose an in-flight count, so
// unlike the IMAP path there is no pending_count to surface.)
func (s *Server) Shutdown(ctx context.Context) (forced bool) {
	err := s.inner.Shutdown(ctx)
	if err == nil {
		return false
	}
	// Deadline (or cancellation) hit with requests still draining: force-close.
	_ = s.inner.Close()
	return true
}
