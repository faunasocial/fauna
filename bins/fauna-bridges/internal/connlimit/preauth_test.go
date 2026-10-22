package connlimit

import (
	"net"
	"sync"
	"testing"
	"time"
)

// recordingConn is a fake inner net.Conn recording every deadline call; a
// read returns a timeout error when readErr is set.
type recordingConn struct {
	net.Conn // nil: only the overridden methods are called
	mu       sync.Mutex
	read     []time.Time
	write    []time.Time
	readErr  error
}

func (c *recordingConn) SetReadDeadline(t time.Time) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.read = append(c.read, t)
	return nil
}

func (c *recordingConn) SetWriteDeadline(t time.Time) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.write = append(c.write, t)
	return nil
}

func (c *recordingConn) Read([]byte) (int, error) { return 0, c.readErr }

func (c *recordingConn) lastRead() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.read[len(c.read)-1]
}

// oneShotListener hands out one prepared conn.
type oneShotListener struct {
	net.Listener
	conn net.Conn
}

func (l *oneShotListener) Accept() (net.Conn, error) { return l.conn, nil }

// wrapper is a fake NetConn() layer (*tls.Conn, the go-imap Conn).
type wrapper struct {
	net.Conn
	inner net.Conn
}

func (w wrapper) NetConn() net.Conn { return w.inner }

type timeoutErr struct{}

func (timeoutErr) Error() string   { return "i/o timeout" }
func (timeoutErr) Timeout() bool   { return true }
func (timeoutErr) Temporary() bool { return true }

func acceptAt(t *testing.T, now time.Time, inner *recordingConn, onDeadline func()) (net.Conn, *time.Time) {
	t.Helper()
	clock := now
	l := NewPreAuthListener(&oneShotListener{conn: inner}, onDeadline)
	l.now = func() time.Time { return clock }
	c, err := l.Accept()
	if err != nil {
		t.Fatalf("accept: %v", err)
	}
	return c, &clock
}

// TestPreAuthAcceptSetsTheBound: Accept sets the inner read deadline to
// accept + PreAuthDeadline before returning the conn.
func TestPreAuthAcceptSetsTheBound(t *testing.T) {
	now := time.Date(2026, 10, 20, 12, 0, 0, 0, time.UTC)
	inner := &recordingConn{}
	acceptAt(t, now, inner, nil)
	if got, want := inner.lastRead(), now.Add(PreAuthDeadline); !got.Equal(want) {
		t.Fatalf("accept read deadline = %v, want %v", got, want)
	}
}

// TestPreAuthClampsUntilAuthenticated: pre-auth, a clear and a later
// deadline keep ours and an earlier one passes; MarkAuthenticated restores
// the library's last request (zero → cleared) and later calls pass through.
func TestPreAuthClampsUntilAuthenticated(t *testing.T) {
	now := time.Date(2026, 10, 20, 12, 0, 0, 0, time.UTC)
	bound := now.Add(PreAuthDeadline)
	inner := &recordingConn{}
	c, _ := acceptAt(t, now, inner, nil)

	_ = c.SetReadDeadline(time.Time{})
	if got := inner.lastRead(); !got.Equal(bound) {
		t.Fatalf("pre-auth clear: inner = %v, want the bound %v", got, bound)
	}
	_ = c.SetReadDeadline(now.Add(30 * time.Minute))
	if got := inner.lastRead(); !got.Equal(bound) {
		t.Fatalf("pre-auth later deadline: inner = %v, want the bound %v", got, bound)
	}
	poke := now.Add(time.Second)
	_ = c.SetReadDeadline(poke)
	if got := inner.lastRead(); !got.Equal(poke) {
		t.Fatalf("pre-auth earlier poke: inner = %v, want it passed %v", got, poke)
	}

	// The library's last request was a clear: authentication restores it.
	_ = c.SetReadDeadline(time.Time{})
	MarkAuthenticated(c)
	if got := inner.lastRead(); !got.IsZero() {
		t.Fatalf("after MarkAuthenticated: inner = %v, want cleared", got)
	}
	later := now.Add(30 * time.Minute)
	_ = c.SetReadDeadline(later)
	if got := inner.lastRead(); !got.Equal(later) {
		t.Fatalf("post-auth: inner = %v, want passed through %v", got, later)
	}
}

// TestPreAuthMarkRestoresANonZeroRequest: a remembered non-zero request is
// what authentication re-applies.
func TestPreAuthMarkRestoresANonZeroRequest(t *testing.T) {
	now := time.Date(2026, 10, 20, 12, 0, 0, 0, time.UTC)
	inner := &recordingConn{}
	c, _ := acceptAt(t, now, inner, nil)
	idle := now.Add(30 * time.Minute)
	_ = c.SetReadDeadline(idle)
	MarkAuthenticated(c)
	if got := inner.lastRead(); !got.Equal(idle) {
		t.Fatalf("after MarkAuthenticated: inner = %v, want the remembered %v", got, idle)
	}
}

// TestPreAuthSetDeadlineSplits: SetDeadline passes the write half untouched
// and clamps the read half.
func TestPreAuthSetDeadlineSplits(t *testing.T) {
	now := time.Date(2026, 10, 20, 12, 0, 0, 0, time.UTC)
	inner := &recordingConn{}
	c, _ := acceptAt(t, now, inner, nil)
	far := now.Add(time.Hour)
	_ = c.SetDeadline(far)
	if len(inner.write) != 1 || !inner.write[0].Equal(far) {
		t.Fatalf("write deadlines = %v, want [%v]", inner.write, far)
	}
	if got := inner.lastRead(); !got.Equal(now.Add(PreAuthDeadline)) {
		t.Fatalf("read half = %v, want the bound", got)
	}
}

// TestPreAuthMarkWalksTheNetConnChain: MarkAuthenticated finds the wrapper
// through NetConn() layers and is a no-op on a chain holding none.
func TestPreAuthMarkWalksTheNetConnChain(t *testing.T) {
	now := time.Date(2026, 10, 20, 12, 0, 0, 0, time.UTC)
	inner := &recordingConn{}
	c, _ := acceptAt(t, now, inner, nil)
	MarkAuthenticated(wrapper{inner: wrapper{inner: c}})
	if got := inner.lastRead(); !got.IsZero() {
		t.Fatalf("through the chain: inner = %v, want cleared", got)
	}

	bare := &recordingConn{}
	MarkAuthenticated(wrapper{inner: bare})
	MarkAuthenticated(nil)
	if len(bare.read) != 0 {
		t.Fatalf("bare chain touched: %v", bare.read)
	}
}

// TestPreAuthCountsOnlyOurTimeout: a read timeout past our bound while
// unauthenticated fires onDeadline once; one before the bound (a library
// poke) does not, nor one after authentication.
func TestPreAuthCountsOnlyOurTimeout(t *testing.T) {
	now := time.Date(2026, 10, 20, 12, 0, 0, 0, time.UTC)
	inner := &recordingConn{readErr: timeoutErr{}}
	fired := 0
	c, clock := acceptAt(t, now, inner, func() { fired++ })
	buf := make([]byte, 1)

	_, _ = c.Read(buf)
	if fired != 0 {
		t.Fatalf("a timeout before the bound counted: %d", fired)
	}
	*clock = now.Add(PreAuthDeadline)
	_, _ = c.Read(buf)
	_, _ = c.Read(buf)
	if fired != 1 {
		t.Fatalf("our timeout counted %d times, want 1", fired)
	}

	inner2 := &recordingConn{readErr: timeoutErr{}}
	fired = 0
	c2, clock2 := acceptAt(t, now, inner2, func() { fired++ })
	MarkAuthenticated(c2)
	*clock2 = now.Add(2 * PreAuthDeadline)
	_, _ = c2.Read(buf)
	if fired != 0 {
		t.Fatalf("a post-auth timeout counted: %d", fired)
	}
}
