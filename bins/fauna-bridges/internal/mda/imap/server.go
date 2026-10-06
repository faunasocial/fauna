package imap

import (
	"context"
	"crypto/tls"
	"net"

	"github.com/emersion/go-imap/v2/imapserver"
)

// ServerConfig collects the per-server settings the MDA wires when
// constructing an IMAP listener. TLSConfig is shared by both the 993
// (implicit-TLS) and 143 (STARTTLS) variants — implicit-TLS sets it
// on the listener via tls.Listen; STARTTLS hands it to imapserver via
// Options.TLSConfig so the library upgrades the conn in-band.
type ServerConfig struct {
	// TLSConfig drives STARTTLS for 143 listeners. Implicit-TLS 993
	// listeners do not consult this — the listener itself is already
	// TLS via tls.Listen(...). Leave nil to disable STARTTLS.
	TLSConfig *tls.Config
}

// Backend produces one imapserver.Session per IMAP TCP connection.
// The production implementation lands in C.2 and wires a Session that
// owns the AUTH'd actor's MLS-decryption capability + nest WS-RPC
// client. C.1 ships the interface plus a fakeBackend in
// server_test.go that the pre-auth tests use.
type Backend interface {
	NewSession(c *imapserver.Conn) imapserver.Session
}

// Server wraps imapserver.Server with the Fauna-specific capability
// set + Backend wiring. One Server instance per listening address;
// main.go starts two of them (993 implicit TLS + 143 STARTTLS) and
// shares the Backend.
type Server struct {
	inner   *imapserver.Server
	backend Backend
}

// NewServer constructs an IMAP server bound to the given Backend.
//
// Per imap-server.md § Authentication: InsecureAuth=false on every
// listener. PLAIN / OAUTHBEARER are not advertised pre-STARTTLS on
// 143; on 993 they surface from the start. emersion/go-imap enforces
// the pre-STARTTLS hiding via its standard pre-TLS-capability flow
// (it returns LOGINDISABLED + omits AUTH=* values until the
// connection is TLS-secured).
func NewServer(cfg ServerConfig, backend Backend) *Server {
	if backend == nil {
		panic("imap.NewServer: backend must not be nil")
	}
	opts := &imapserver.Options{
		NewSession: func(c *imapserver.Conn) (imapserver.Session, *imapserver.GreetingData, error) {
			return backend.NewSession(c), &imapserver.GreetingData{PreAuth: false}, nil
		},
		Caps:         capabilityList(),
		TLSConfig:    cfg.TLSConfig,
		InsecureAuth: false,
	}
	return &Server{
		inner:   imapserver.New(opts),
		backend: backend,
	}
}

// Serve accepts connections on the listener and processes them until
// the listener is closed or Close is called. Standard io.EOF-on-close
// semantics — callers running Serve in a goroutine ignore the error
// after Close.
func (s *Server) Serve(ln net.Listener) error {
	return s.inner.Serve(ln)
}

// Close stops accepting new connections and waits for in-flight
// sessions to terminate. Each Session's Close method runs as part of
// teardown; the Backend is responsible for zeroizing per-session
// secrets (mlocked MLS capability, etc) in its Session.Close.
func (s *Server) Close() error {
	return s.inner.Close()
}

// Shutdown gracefully drains the server within ctx (T2.6,
// mail-bridge-lifecycle.md § Shutting down): stop accepting, `* BYE` new
// commands and promptly evict idle/IDLE'ing sessions, drain in-flight
// commands up to the deadline, then force-close. Returns whether it had to
// force-close stragglers and how many were still in flight at that point —
// the MDA role logs the count and maps forced=true to
// bridgeshutdown.ErrShutdownForced. Delegates to the fork's graceful
// Shutdown (third_party/go-imap, FORK.md row 18).
func (s *Server) Shutdown(ctx context.Context) (forced bool, pending int) {
	return s.inner.Shutdown(ctx)
}
