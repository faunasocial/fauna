// Package connlimit caps the number of simultaneously-served connections on
// a net.Listener with a counting semaphore. Accept acquires a slot before
// pulling a connection off the queue, so connections beyond the cap wait in
// the kernel accept backlog rather than spawning unbounded goroutines; each
// returned connection releases its slot on the first Close.
//
// Lifted verbatim from the port-25 MTA listener (internal/mta/server.go) so
// every bridge listener — inbound MX, submission, IMAP, CalDAV — shares one
// connection-cap shape (security.md / smtp-server.md § Connection-time
// limits). Callers choose the cap appropriate to their surface: the
// unauthenticated MX port stays deliberately low (back-pressure botnet
// bursts) while the authenticated submission / IMAP / CalDAV surfaces use a
// generous OS-FD backstop (IMAP in particular holds many persistent IDLE
// connections).
//
// The wrapper overrides only Accept; Close, Addr, and the rest are promoted
// from the embedded net.Listener. In particular a graceful-drain path that
// closes the wrapped listener (http.Server.Shutdown, go-imap Shutdown,
// gosmtp gracefulDrain) closes the inner listener unchanged.
package connlimit

import (
	"net"
	"sync"
	"time"
)

// CapWarnDelay is how long an inbound Accept may block on the concurrency
// semaphore before the connection is counted as a `capped` event (sustained
// saturation, distinct from momentary contention). A var, not a const, so
// tests can shrink it for deterministic assertions.
var CapWarnDelay = 1 * time.Second

// Listener wraps a net.Listener with a global concurrent-connection cap.
// Accept acquires a semaphore slot before pulling a connection off the
// queue (so a connection we can't service stays in the kernel backlog
// rather than spawning a goroutine); the returned connection releases the
// slot on Close. onCapped fires once when an Accept blocks past
// CapWarnDelay (sustained saturation); onAccept fires per accept. Both
// callbacks are nil-safe so the type is usable without metrics.
type Listener struct {
	net.Listener
	sem      chan struct{}
	onCapped func()
	onAccept func()
}

// New wraps inner with a cap of max simultaneously-served connections.
// onAccept fires per successful accept and onCapped fires once each time an
// Accept blocks past CapWarnDelay; either may be nil.
func New(inner net.Listener, max int, onAccept, onCapped func()) *Listener {
	return &Listener{
		Listener: inner,
		sem:      make(chan struct{}, max),
		onAccept: onAccept,
		onCapped: onCapped,
	}
}

// Accept acquires a slot before accepting. Fast path: a free slot is taken
// immediately. Slow path: block, and if the wait crosses CapWarnDelay count
// one `capped` event before continuing to wait for a slot.
func (l *Listener) Accept() (net.Conn, error) {
	select {
	case l.sem <- struct{}{}:
	default:
		timer := time.NewTimer(CapWarnDelay)
		select {
		case l.sem <- struct{}{}:
			timer.Stop()
		case <-timer.C:
			if l.onCapped != nil {
				l.onCapped()
			}
			l.sem <- struct{}{} // keep waiting until a slot frees
		}
	}
	conn, err := l.Listener.Accept()
	if err != nil {
		<-l.sem
		return nil, err
	}
	if l.onAccept != nil {
		l.onAccept()
	}
	return &slotConn{Conn: conn, release: func() { <-l.sem }}, nil
}

// slotConn releases its listener slot exactly once, on the first Close.
type slotConn struct {
	net.Conn
	once    sync.Once
	release func()
}

func (c *slotConn) Close() error {
	err := c.Conn.Close()
	c.once.Do(c.release)
	return err
}
