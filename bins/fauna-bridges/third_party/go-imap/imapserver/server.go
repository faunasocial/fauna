// Package imapserver implements an IMAP server.
package imapserver

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"sync"
	"sync/atomic"
	"time"

	"github.com/emersion/go-imap/v2"
)

var errClosed = errors.New("imapserver: server closed")

// Logger is a facility to log error messages.
type Logger interface {
	Printf(format string, args ...interface{})
}

// Options contains server options.
//
// The only required field is NewSession.
type Options struct {
	// NewSession is called when a client connects.
	NewSession func(*Conn) (Session, *GreetingData, error)
	// Supported capabilities. If nil, only IMAP4rev1 is advertised. This set
	// must contain at least IMAP4rev1 or IMAP4rev2.
	//
	// The following capabilities are part of IMAP4rev2 and need to be
	// explicitly enabled by IMAP4rev1-only servers:
	//
	//   - NAMESPACE
	//   - UIDPLUS
	//   - ESEARCH
	//   - LIST-EXTENDED
	//   - LIST-STATUS
	//   - MOVE
	//   - STATUS=SIZE
	Caps imap.CapSet
	// Logger is a logger to print error messages. If nil, log.Default is used.
	Logger Logger
	// TLSConfig is a TLS configuration for STARTTLS. If nil, STARTTLS is
	// disabled.
	TLSConfig *tls.Config
	// InsecureAuth allows clients to authenticate without TLS. In this mode,
	// the server is susceptible to man-in-the-middle attacks.
	InsecureAuth bool
	// Raw ingress and egress data will be written to this writer, if any.
	// Note, this may include sensitive information such as credentials used
	// during authentication.
	DebugWriter io.Writer
}

func (options *Options) wrapReadWriter(rw io.ReadWriter) io.ReadWriter {
	if options.DebugWriter == nil {
		return rw
	}
	return struct {
		io.Reader
		io.Writer
	}{
		Reader: io.TeeReader(rw, options.DebugWriter),
		Writer: io.MultiWriter(rw, options.DebugWriter),
	}
}

func (options *Options) caps() imap.CapSet {
	if options.Caps != nil {
		return options.Caps
	}
	return imap.CapSet{imap.CapIMAP4rev1: {}}
}

// Server is an IMAP server.
type Server struct {
	options Options

	listenerWaitGroup sync.WaitGroup

	// FAUNA-FORK: draining is set by Shutdown to make the per-connection
	// command loop send an untagged `* BYE` and close instead of reading the
	// next command (RFC 9051 §7.1.5). Read on every command-loop turn, so an
	// atomic rather than the mutex below.
	draining atomic.Bool

	mutex     sync.Mutex
	listeners map[net.Listener]struct{}
	conns     map[*Conn]struct{}
	closed    bool
}

// shuttingDown reports whether Shutdown has begun draining this server.
//
// FAUNA-FORK: consulted by the connection command loop (conn.go) to BYE new
// commands during a graceful shutdown.
func (s *Server) shuttingDown() bool {
	return s.draining.Load()
}

// New creates a new server.
func New(options *Options) *Server {
	if caps := options.caps(); !caps.Has(imap.CapIMAP4rev2) && !caps.Has(imap.CapIMAP4rev1) {
		panic("imapserver: at least IMAP4rev1 must be supported")
	}
	return &Server{
		options:   *options,
		listeners: make(map[net.Listener]struct{}),
		conns:     make(map[*Conn]struct{}),
	}
}

func (s *Server) logger() Logger {
	if s.options.Logger == nil {
		return log.Default()
	}
	return s.options.Logger
}

// Serve accepts incoming connections on the listener ln.
func (s *Server) Serve(ln net.Listener) error {
	s.mutex.Lock()
	ok := !s.closed
	if ok {
		s.listeners[ln] = struct{}{}
	}
	s.mutex.Unlock()
	if !ok {
		return errClosed
	}

	defer func() {
		s.mutex.Lock()
		delete(s.listeners, ln)
		s.mutex.Unlock()
	}()

	s.listenerWaitGroup.Add(1)
	defer s.listenerWaitGroup.Done()

	var delay time.Duration
	for {
		conn, err := ln.Accept()
		if ne, ok := err.(net.Error); ok && ne.Temporary() {
			if delay == 0 {
				delay = 5 * time.Millisecond
			} else {
				delay *= 2
			}
			if max := 1 * time.Second; delay > max {
				delay = max
			}
			s.logger().Printf("accept error (retrying in %v): %v", delay, err)
			time.Sleep(delay)
			continue
		} else if errors.Is(err, net.ErrClosed) {
			return nil
		} else if err != nil {
			return fmt.Errorf("accept error: %w", err)
		}

		delay = 0
		go newConn(conn, s).serve()
	}
}

// ListenAndServe listens on the TCP network address addr and then calls Serve.
//
// If addr is empty, ":143" is used.
func (s *Server) ListenAndServe(addr string) error {
	if addr == "" {
		addr = ":143"
	}
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		return err
	}
	return s.Serve(ln)
}

// ListenAndServeTLS listens on the TCP network address addr and then calls
// Serve to handle incoming TLS connections.
//
// The TLS configuration set in Options.TLSConfig is used. If addr is empty,
// ":993" is used.
func (s *Server) ListenAndServeTLS(addr string) error {
	if addr == "" {
		addr = ":993"
	}
	ln, err := tls.Listen("tcp", addr, s.options.TLSConfig)
	if err != nil {
		return err
	}
	return s.Serve(ln)
}

// Close immediately closes all active listeners and connections.
//
// Close returns any error returned from closing the server's underlying
// listeners.
//
// Once Close has been called on a server, it may not be reused; future calls
// to methods such as Serve will return an error.
func (s *Server) Close() error {
	var err error

	s.mutex.Lock()
	ok := !s.closed
	if ok {
		s.closed = true
		for l := range s.listeners {
			if closeErr := l.Close(); closeErr != nil && err == nil {
				err = closeErr
			}
		}
	}
	s.mutex.Unlock()
	if !ok {
		return errClosed
	}

	s.listenerWaitGroup.Wait()

	s.forceCloseConns()

	return err
}

// forceCloseConns force-closes every currently-tracked connection. Each
// connection's serve goroutine then unblocks on its next read with
// net.ErrClosed and removes itself from s.conns.
//
// FAUNA-FORK: extracted from Close so the graceful Shutdown path can reuse it
// for the stragglers left when the grace window expires.
func (s *Server) forceCloseConns() {
	s.mutex.Lock()
	for c := range s.conns {
		c.mutex.Lock()
		c.conn.Close()
		c.mutex.Unlock()
	}
	s.mutex.Unlock()
}

// Shutdown gracefully shuts the server down within the deadline carried by
// ctx, returning whether it had to force-close stragglers and how many were
// still in flight at that point.
//
// FAUNA-FORK: additive graceful-shutdown seam (upstream beta.8 only ships the
// force-only Close). Mirrors net/http.Server.Shutdown's idle/active split, but
// for the IMAP idiom: a connection blocked between commands or parked in IDLE
// is woken and sent `* BYE` immediately (RFC 9051 §7.1.5 — don't wait the full
// grace for a long-lived IDLE session), while a connection mid-command (e.g.
// draining an APPEND literal or building a FETCH response) is left to finish
// up to the deadline before being force-closed. The bridge's MDA role
// (internal/mda) maps a forced result to bridgeshutdown.ErrShutdownForced;
// the goal-doc contract is docs/goal/behavior/mail-bridge-lifecycle.md
// § Shutting down. Tracked in FORK.md.
//
// Sequence:
//  1. mark the server draining + closed so the command loop BYEs and Serve
//     refuses re-use;
//  2. close listeners (stop accepting) and wait for the Serve loops to return;
//  3. poke a now read-deadline on every connection NOT mid-command so a
//     blocked tag-read / IDLE wait wakes, sees draining, and BYEs promptly;
//  4. wait up to ctx for the connection set to empty;
//  5. on deadline, force-close the stragglers and report the pending count.
func (s *Server) Shutdown(ctx context.Context) (forced bool, pending int) {
	s.mutex.Lock()
	if s.closed {
		s.mutex.Unlock()
		return false, 0
	}
	s.closed = true
	s.draining.Store(true)
	for l := range s.listeners {
		_ = l.Close()
	}
	// Snapshot the connections to wake: those NOT actively processing a
	// command (idle between commands, or parked in IDLE). Connections
	// mid-command are left to drain — interrupting their read would cut an
	// in-flight APPEND/literal, the IMAP analog of an in-flight SMTP DATA.
	var idle []*Conn
	for c := range s.conns {
		if !c.inFlight.Load() {
			idle = append(idle, c)
		}
	}
	s.mutex.Unlock()

	// Serve loops have returned (their listeners are closed); no new
	// connections will be accepted from here on.
	s.listenerWaitGroup.Wait()

	now := time.Now()
	for _, c := range idle {
		c.mutex.Lock()
		_ = c.conn.SetReadDeadline(now)
		c.mutex.Unlock()
	}

	if s.waitConnsDrained(ctx) {
		return false, 0
	}

	s.mutex.Lock()
	pending = len(s.conns)
	s.mutex.Unlock()
	s.forceCloseConns()
	return true, pending
}

// waitConnsDrained blocks until every tracked connection has removed itself
// (the command loops exit on BYE / logout / read error) or ctx is done.
// Returns true iff the set drained within ctx.
//
// FAUNA-FORK: poll-based wait for the graceful Shutdown path. The shutdown
// window is short and the poll interval small, so a ticker is simpler than a
// dedicated sync.Cond + a broadcast in the hot per-connection teardown path.
func (s *Server) waitConnsDrained(ctx context.Context) bool {
	ticker := time.NewTicker(20 * time.Millisecond)
	defer ticker.Stop()
	for {
		s.mutex.Lock()
		n := len(s.conns)
		s.mutex.Unlock()
		if n == 0 {
			return true
		}
		select {
		case <-ctx.Done():
			s.mutex.Lock()
			n := len(s.conns)
			s.mutex.Unlock()
			return n == 0
		case <-ticker.C:
		}
	}
}
