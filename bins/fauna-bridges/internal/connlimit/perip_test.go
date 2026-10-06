package connlimit

import (
	"net"
	"sync/atomic"
	"testing"
)

func ip(s string) net.IP { return net.ParseIP(s) }

func TestPerIPLimiter_AdmitsUpToCapThenSheds(t *testing.T) {
	l := NewPerIPLimiter(2)
	a := ip("203.0.113.7")
	r1, ok1 := l.tryAcquire(a)
	r2, ok2 := l.tryAcquire(a)
	if !ok1 || !ok2 {
		t.Fatalf("first two from one IP must be admitted, got %v %v", ok1, ok2)
	}
	if _, ok := l.tryAcquire(a); ok {
		t.Fatal("third from the same IP must be shed at cap")
	}
	// Releasing one frees a slot.
	r1()
	if _, ok := l.tryAcquire(a); !ok {
		t.Fatal("after a release, the IP is re-admittable")
	}
	_ = r2
}

func TestPerIPLimiter_ReleaseGCsTheEntry(t *testing.T) {
	l := NewPerIPLimiter(1)
	a := ip("203.0.113.7")
	r1, ok := l.tryAcquire(a)
	if !ok {
		t.Fatal("first must be admitted")
	}
	if _, ok := l.tryAcquire(a); ok {
		t.Fatal("second must be shed at cap 1")
	}
	r1()
	if got := l.trackedIPs(); got != 0 {
		t.Fatalf("entry must be GC'd when its count hits zero, tracked=%d", got)
	}
}

func TestPerIPLimiter_DistinctIPsIndependent(t *testing.T) {
	l := NewPerIPLimiter(1)
	if _, ok := l.tryAcquire(ip("203.0.113.7")); !ok {
		t.Fatal("first IP admitted")
	}
	if _, ok := l.tryAcquire(ip("203.0.113.8")); !ok {
		t.Fatal("a different source does not share the cap")
	}
}

// The Go mirror of the Rust crate's 2026-08-22 regression lock: loopback used
// to be exempt outright; it is now counted against its own ceiling.
func TestPerIPLimiter_LoopbackBoundedByItsOwnCeiling(t *testing.T) {
	l := NewPerIPLimiter(1)
	l.loopbackMax = 3
	lo := ip("127.0.0.1")
	var releases []func()
	for i := 0; i < 3; i++ {
		r, ok := l.tryAcquire(lo)
		if !ok {
			t.Fatalf("loopback admitted up to its ceiling, past the admin cap of 1 (iter %d)", i)
		}
		releases = append(releases, r)
	}
	if _, ok := l.tryAcquire(lo); ok {
		t.Fatal("the connection past the loopback ceiling must be shed")
	}
	if got := l.trackedIPs(); got != 1 {
		t.Fatalf("loopback IS tracked, tracked=%d", got)
	}
	for _, r := range releases {
		r()
	}
	if got := l.trackedIPs(); got != 0 {
		t.Fatalf("loopback entry GC'd on release, tracked=%d", got)
	}
	if _, ok := l.tryAcquire(lo); !ok {
		t.Fatal("released slots are re-admittable")
	}
	if got := l.CeilingFor(net.ParseIP("::1")); got != 3 {
		t.Fatalf("IPv6 loopback bounded by the same ceiling, got %d", got)
	}
}

func TestPerIPLimiter_LoopbackCeilingIndependentOfAdminCap(t *testing.T) {
	l := NewPerIPLimiter(0) // admin cap DISABLED
	l.loopbackMax = 2
	lo, ext := ip("127.0.0.1"), ip("203.0.113.7")
	for i := 0; i < 5; i++ {
		if _, ok := l.tryAcquire(ext); !ok {
			t.Fatalf("disabled cap admits every external connection (iter %d)", i)
		}
	}
	if _, ok := l.tryAcquire(lo); !ok {
		t.Fatal("loopback admitted under its ceiling while the cap is disabled")
	}
	if _, ok := l.tryAcquire(lo); !ok {
		t.Fatal("loopback admitted under its ceiling while the cap is disabled")
	}
	if _, ok := l.tryAcquire(lo); ok {
		t.Fatal("loopback still shed at its own ceiling while the admin cap is disabled")
	}
	l.SetMax(10000)
	if _, ok := l.tryAcquire(lo); ok {
		t.Fatal("widening the admin cap must not widen the loopback ceiling")
	}
	if got := l.CeilingFor(lo); got != 2 {
		t.Fatalf("CeilingFor(loopback)=%d, want 2", got)
	}
	if got := l.CeilingFor(ext); got != 10000 {
		t.Fatalf("CeilingFor(external)=%d, want the admin's live cap", got)
	}
	if NewPerIPLimiter(DefaultMaxConnsPerIP).CeilingFor(lo) != LoopbackMaxConns {
		t.Fatal("the production constructor uses LoopbackMaxConns")
	}
}

func TestPerIPLimiter_DisabledAdmitsAll(t *testing.T) {
	l := NewPerIPLimiter(0) // 0 = disabled (the catalog sentinel)
	a := ip("203.0.113.7")
	for i := 0; i < 1000; i++ {
		if _, ok := l.tryAcquire(a); !ok {
			t.Fatalf("a disabled limiter admits everything (iter %d)", i)
		}
	}
	if got := l.trackedIPs(); got != 0 {
		t.Fatalf("a disabled limiter tracks nothing, tracked=%d", got)
	}
}

func TestPerIPLimiter_SetMaxHotReload(t *testing.T) {
	l := NewPerIPLimiter(1)
	a := ip("203.0.113.7")
	if _, ok := l.tryAcquire(a); !ok {
		t.Fatal("first admitted at cap 1")
	}
	if _, ok := l.tryAcquire(a); ok {
		t.Fatal("second shed at cap 1")
	}
	l.SetMax(3) // admin raises the ceiling on a config_changed push
	if _, ok := l.tryAcquire(a); !ok {
		t.Fatal("after raising the cap, the next is admitted (already 1 held → 2 ≤ 3)")
	}
	l.SetMax(0) // disable
	for i := 0; i < 5; i++ {
		if _, ok := l.tryAcquire(a); !ok {
			t.Fatalf("after disabling, everything is admitted (iter %d)", i)
		}
	}
}

func TestPerIPLimiter_NilIPExempt(t *testing.T) {
	l := NewPerIPLimiter(1)
	if _, ok := l.tryAcquire(nil); !ok {
		t.Fatal("a nil (unparseable) source IP fails open")
	}
}

// --- listener-level shed behavior ---

// fakeConn is a minimal net.Conn whose RemoteAddr is fully controllable, so a
// PerIPListener can be exercised with non-loopback sources (a real loopback
// listener would only ever see exempt loopback peers).
type fakeConn struct {
	net.Conn
	remote net.Addr
	closed atomic.Bool
}

func (c *fakeConn) RemoteAddr() net.Addr { return c.remote }
func (c *fakeConn) Close() error         { c.closed.Store(true); return nil }

// fakeListener yields a fixed queue of conns, then blocks (a real listener
// blocks on Accept until the next connection); the test only enqueues what it
// needs and never drains past it.
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

func TestPerIPListener_ShedsOverCapAndCountsThem(t *testing.T) {
	// Three connections from one source IP, cap 2: the third is shed (closed,
	// onShed fired) and the listener returns the FOURTH (a different IP).
	c1 := &fakeConn{remote: tcpAddr("203.0.113.7")}
	c2 := &fakeConn{remote: tcpAddr("203.0.113.7")}
	c3 := &fakeConn{remote: tcpAddr("203.0.113.7")} // over cap → shed
	c4 := &fakeConn{remote: tcpAddr("198.51.100.9")}
	inner := newFakeListener(c1, c2, c3, c4)

	var shed atomic.Int32
	l := NewPerIPListener(inner, NewPerIPLimiter(2), func() { shed.Add(1) })

	got1, _ := l.Accept()
	got2, _ := l.Accept()
	// The next Accept sheds c3 internally and returns c4.
	got3, _ := l.Accept()

	if shed.Load() != 1 {
		t.Fatalf("exactly one connection should be shed, got %d", shed.Load())
	}
	if !c3.closed.Load() {
		t.Fatal("the over-cap connection must be closed")
	}
	if got3.RemoteAddr().(*net.TCPAddr).IP.String() != "198.51.100.9" {
		t.Fatalf("Accept should skip the shed conn and return the next IP, got %v", got3.RemoteAddr())
	}
	// Closing an admitted conn frees the source's slot.
	_ = got1.Close()
	_ = got2.Close()
}

func TestPerIPListener_DisabledNeverSheds(t *testing.T) {
	c1 := &fakeConn{remote: tcpAddr("203.0.113.7")}
	c2 := &fakeConn{remote: tcpAddr("203.0.113.7")}
	c3 := &fakeConn{remote: tcpAddr("203.0.113.7")}
	inner := newFakeListener(c1, c2, c3)
	var shed atomic.Int32
	l := NewPerIPListener(inner, NewPerIPLimiter(0), func() { shed.Add(1) }) // disabled
	for i := 0; i < 3; i++ {
		if _, err := l.Accept(); err != nil {
			t.Fatalf("accept %d: %v", i, err)
		}
	}
	if shed.Load() != 0 {
		t.Fatalf("a disabled limiter sheds nothing, got %d", shed.Load())
	}
}

func TestIpOf(t *testing.T) {
	if got := ipOf(&net.TCPAddr{IP: net.ParseIP("203.0.113.7"), Port: 25}); got.String() != "203.0.113.7" {
		t.Fatalf("TCPAddr → %v", got)
	}
	if got := ipOf(nil); got != nil {
		t.Fatalf("nil addr → %v, want nil", got)
	}
	// A non-TCP addr string form still parses host:port.
	ua := &net.UDPAddr{IP: net.ParseIP("198.51.100.9"), Port: 53}
	if got := ipOf(ua); got == nil || got.String() != "198.51.100.9" {
		t.Fatalf("UDPAddr fallback → %v", got)
	}
}
