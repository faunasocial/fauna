package mta

import (
	"net"
	"sync/atomic"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/connlimit"
)

// fakeConn is a minimal net.Conn with a controllable RemoteAddr, so the port-25
// per-IP listener can be exercised with non-loopback sources (a real loopback
// listener only ever sees exempt loopback peers). Mirrors the connlimit-package
// test helper — Go test code can't be shared across package boundaries.
type fakeConn struct {
	net.Conn
	remote net.Addr
	closed atomic.Bool
}

func (c *fakeConn) RemoteAddr() net.Addr { return c.remote }
func (c *fakeConn) Close() error         { c.closed.Store(true); return nil }

// fakeListener yields a fixed queue of conns, then blocks (a real listener blocks
// on Accept until the next connection); a test only enqueues what it needs.
type fakeListener struct {
	net.Listener
	ch chan net.Conn
}

func newFakeListener(conns ...net.Conn) *fakeListener {
	ch := make(chan net.Conn, len(conns))
	for _, c := range conns {
		ch <- c
	}
	return &fakeListener{ch: ch}
}

func (l *fakeListener) Accept() (net.Conn, error) { return <-l.ch, nil }
func (l *fakeListener) Close() error              { return nil }

func tcpAddr(ipStr string) net.Addr {
	return &net.TCPAddr{IP: net.ParseIP(ipStr), Port: 54321}
}

// TestPerIPInbound_ShedsOverCapAdmitsDifferentIP proves the port-25 per-IP
// concurrent cap (perIPInbound) sheds an over-cap connection from one source IP
// while still admitting a different IP — so a single (or looping) source cannot
// pin all maxInboundConns global :25 slots, the trickle-slowloris defense beside
// port 25's per-IP *rate* cap (10/min) and the 30 s per-command read timeout. The
// cap is overridden small here for brevity; the production value is
// maxInboundConnsPerIP (guarded by TestMaxInboundConnsPerIP_IsFractionOfGlobalCap).
func TestPerIPInbound_ShedsOverCapAdmitsDifferentIP(t *testing.T) {
	c1 := &fakeConn{remote: tcpAddr("203.0.113.7")}
	c2 := &fakeConn{remote: tcpAddr("203.0.113.7")}
	c3 := &fakeConn{remote: tcpAddr("203.0.113.7")} // over cap (2) → shed + closed
	other := &fakeConn{remote: tcpAddr("198.51.100.9")}
	inner := newFakeListener(c1, c2, c3, other)

	l := perIPInbound(inner, connlimit.NewPerIPLimiter(2), "25")
	_, _ = l.Accept()    // c1 admitted
	_, _ = l.Accept()    // c2 admitted (203.0.113.7 now at cap)
	got, _ := l.Accept() // c3 is over cap → shed; Accept returns `other`

	if !c3.closed.Load() {
		t.Fatal("the over-cap port-25 connection from one IP must be shed (closed)")
	}
	if ip := got.RemoteAddr().(*net.TCPAddr).IP.String(); ip != "198.51.100.9" {
		t.Fatalf("a different source IP must still be admitted while one IP is capped, got %s", ip)
	}
}

// TestPerIPInbound_LoopbackExempt: the in-container router/bridge dials port 25
// over loopback; those must never be capped (capping them would starve trusted
// internal delivery paths).
func TestPerIPInbound_LoopbackExempt(t *testing.T) {
	c1 := &fakeConn{remote: tcpAddr("127.0.0.1")}
	c2 := &fakeConn{remote: tcpAddr("127.0.0.1")}
	c3 := &fakeConn{remote: tcpAddr("127.0.0.1")}
	inner := newFakeListener(c1, c2, c3)

	l := perIPInbound(inner, connlimit.NewPerIPLimiter(1), "25") // cap 1
	for i := 0; i < 3; i++ {
		if _, err := l.Accept(); err != nil {
			t.Fatalf("loopback accept %d errored: %v", i, err)
		}
	}
	for i, c := range []*fakeConn{c1, c2, c3} {
		if c.closed.Load() {
			t.Fatalf("loopback conn %d must never be shed by the per-IP cap", i)
		}
	}
}

// TestMaxInboundConnsPerIP_IsFractionOfGlobalCap pins the defense-in-depth
// invariant: the per-IP concurrent cap must be a strict, non-zero fraction of the
// global :25 cap, so no single source IP can pin all global slots. Raising it to
// >= maxInboundConns would silently turn the per-IP cap into a no-op and reopen
// the trickle-slowloris residual.
func TestMaxInboundConnsPerIP_IsFractionOfGlobalCap(t *testing.T) {
	if maxInboundConnsPerIP == 0 || maxInboundConnsPerIP >= maxInboundConns {
		t.Fatalf("maxInboundConnsPerIP (%d) must be in (0, maxInboundConns=%d)", maxInboundConnsPerIP, maxInboundConns)
	}
}
