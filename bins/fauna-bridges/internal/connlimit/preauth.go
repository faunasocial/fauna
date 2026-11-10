package connlimit

import (
	"errors"
	"net"
	"sync"
	"time"
)

// PreAuthDeadline bounds an unauthenticated connection's whole life: a
// connection that has not authenticated this long after accept is closed
// (`docs/goal/behavior/smtp-server.md` § Connection-time limits → *Pre-auth
// connection deadline*). Fixed from accept, never reset by a read, so a
// trickle of bytes cannot extend it. A compile-time constant, never a knob:
// the release bridge always applies exactly this (preAuthDeadline in
// preauth_seam_absent.go); only the e2e flavor's harness seam
// (preauth_seam.go, `-tags fauna_e2e_fixtures`) can shorten it.
const PreAuthDeadline = 60 * time.Second

// PreAuthListener wraps a net.Listener so every accepted connection carries
// the pre-auth deadline until its session calls MarkAuthenticated. Wrap it
// outermost below tls.NewListener, so the TLS handshake and STARTTLS run
// under the bound while the serving library still sees a *tls.Conn.
type PreAuthListener struct {
	net.Listener
	onDeadline func()
	// now is the clock; a test-only seam (time.Now in production).
	now func() time.Time
	// deadline is PreAuthDeadline in every release bridge, fixed at
	// construction from preAuthDeadline().
	deadline time.Duration
}

// NewPreAuthListener wraps inner with the pre-auth deadline. onDeadline fires
// once per connection whose read or write fails on OUR bound (the caller's
// metric and log); it may be nil.
func NewPreAuthListener(inner net.Listener, onDeadline func()) *PreAuthListener {
	return &PreAuthListener{Listener: inner, onDeadline: onDeadline, now: time.Now, deadline: preAuthDeadline()}
}

// SetClockForTest replaces the listener's clock — the witnesses' seam for a
// deadline already past at accept without a wall-clock wait. Test-only, never
// a knob.
func (l *PreAuthListener) SetClockForTest(now func() time.Time) { l.now = now }

// Accept sets the inner read AND write deadlines to accept + the deadline
// before returning the wrapped connection. The write half matters as much as
// the read: a stranger that pipelines commands and stops reading leaves the
// server blocked in Write, where no read deadline applies.
func (l *PreAuthListener) Accept() (net.Conn, error) {
	conn, err := l.Listener.Accept()
	if err != nil {
		return nil, err
	}
	c := &preAuthConn{
		Conn:       conn,
		bound:      l.now().Add(l.deadline),
		now:        l.now,
		onDeadline: l.onDeadline,
	}
	if err := conn.SetReadDeadline(c.bound); err != nil {
		_ = conn.Close()
		return nil, err
	}
	if err := conn.SetWriteDeadline(c.bound); err != nil {
		_ = conn.Close()
		return nil, err
	}
	return c, nil
}

// preAuthConn clamps every read and write deadline the serving library sets
// to the pre-auth bound until MarkAuthenticated lifts it. The serving library
// pokes deadlines itself (go-imap's IDLE deadline and drain wake-up,
// go-smtp's per-reply WriteTimeout), so the bound is a clamp, never a
// set-once: a clear or a later deadline keeps ours, an earlier one passes.
// Each half remembers its own last request for the lift.
type preAuthConn struct {
	net.Conn
	bound      time.Time
	now        func() time.Time
	onDeadline func()

	mu             sync.Mutex
	authenticated  bool
	requestedRead  time.Time // the library's last-requested read deadline
	requestedWrite time.Time // the library's last-requested write deadline
	counted        bool
}

// clamped returns the deadline to apply for a requested one while
// unauthenticated. Caller holds mu.
func (c *preAuthConn) clamped(t time.Time) time.Time {
	if t.IsZero() || t.After(c.bound) {
		return c.bound
	}
	return t
}

func (c *preAuthConn) SetReadDeadline(t time.Time) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.authenticated {
		return c.Conn.SetReadDeadline(t)
	}
	c.requestedRead = t
	return c.Conn.SetReadDeadline(c.clamped(t))
}

func (c *preAuthConn) SetWriteDeadline(t time.Time) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.authenticated {
		return c.Conn.SetWriteDeadline(t)
	}
	c.requestedWrite = t
	return c.Conn.SetWriteDeadline(c.clamped(t))
}

func (c *preAuthConn) SetDeadline(t time.Time) error {
	if err := c.SetWriteDeadline(t); err != nil {
		return err
	}
	return c.SetReadDeadline(t)
}

// Read and Write count a timeout on OUR bound — the one place that knows the
// close was ours.
func (c *preAuthConn) Read(p []byte) (int, error) {
	n, err := c.Conn.Read(p)
	c.countIfOurs(err)
	return n, err
}

func (c *preAuthConn) Write(p []byte) (int, error) {
	n, err := c.Conn.Write(p)
	c.countIfOurs(err)
	return n, err
}

// countIfOurs fires onDeadline for a timeout past our bound while
// unauthenticated, once per connection whichever half saw it.
func (c *preAuthConn) countIfOurs(err error) {
	var ne net.Error
	if err == nil || !errors.As(err, &ne) || !ne.Timeout() {
		return
	}
	c.mu.Lock()
	ours := !c.authenticated && !c.counted && !c.now().Before(c.bound)
	if ours {
		c.counted = true
	}
	c.mu.Unlock()
	if ours && c.onDeadline != nil {
		c.onDeadline()
	}
}

// markAuthenticated lifts the clamp and restores the library's own
// last-requested read and write deadlines (zero → cleared).
func (c *preAuthConn) markAuthenticated() {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.authenticated {
		return
	}
	c.authenticated = true
	_ = c.Conn.SetReadDeadline(c.requestedRead)
	_ = c.Conn.SetWriteDeadline(c.requestedWrite)
}

// netConner is the unwrap step: *tls.Conn and go-imap's *imapserver.Conn
// both expose the connection beneath them.
type netConner interface {
	NetConn() net.Conn
}

// MarkAuthenticated lifts the pre-auth deadline on c — the session's one
// auth-success point calls it. It walks c down any NetConn() chain until it
// finds the wrapper; a chain holding none (a nil conn, a unix socket in a
// test) is a silent no-op.
func MarkAuthenticated(c net.Conn) {
	for c != nil {
		if pc, ok := c.(*preAuthConn); ok {
			pc.markAuthenticated()
			return
		}
		nc, ok := c.(netConner)
		if !ok {
			return
		}
		c = nc.NetConn()
	}
}
