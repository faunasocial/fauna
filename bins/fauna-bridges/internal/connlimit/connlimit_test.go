package connlimit

import (
	"net"
	"sync/atomic"
	"testing"
	"time"
)

// TestListenerGatesAtCap drives New directly over a real ephemeral
// listener: with cap=2, the first two Accepts proceed, the third blocks
// past CapWarnDelay (firing onCapped), and only frees once an accepted
// connection is Closed (releasing its slot). Lifted from the port-25 MTA
// listener test when the machinery moved into this package.
func TestListenerGatesAtCap(t *testing.T) {
	old := CapWarnDelay
	CapWarnDelay = 20 * time.Millisecond
	defer func() { CapWarnDelay = old }()

	inner, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer func() { _ = inner.Close() }()

	var capped int32
	ll := New(inner, 2, nil, func() { atomic.AddInt32(&capped, 1) })
	addr := inner.Addr().String()

	type acc struct {
		c   net.Conn
		err error
	}
	accepts := make(chan acc, 4)
	go func() {
		for {
			c, e := ll.Accept()
			accepts <- acc{c, e}
			if e != nil {
				return
			}
		}
	}()

	for i := 0; i < 3; i++ {
		c, dErr := net.Dial("tcp", addr)
		if dErr != nil {
			t.Fatalf("dial %d: %v", i, dErr)
		}
		defer func() { _ = c.Close() }()
	}

	var accepted []net.Conn
	for i := 0; i < 2; i++ {
		select {
		case a := <-accepts:
			if a.err != nil {
				t.Fatalf("accept %d: %v", i, a.err)
			}
			accepted = append(accepted, a.c)
		case <-time.After(time.Second):
			t.Fatalf("accept %d timed out at cap=2", i)
		}
	}

	// Third accept must be gated.
	select {
	case a := <-accepts:
		t.Fatalf("third accept returned while at cap (conn=%v err=%v)", a.c, a.err)
	case <-time.After(150 * time.Millisecond):
	}
	if atomic.LoadInt32(&capped) == 0 {
		t.Errorf("onCapped never fired despite a >CapWarnDelay block")
	}

	// Free a slot → the gated accept proceeds.
	_ = accepted[0].Close()
	select {
	case a := <-accepts:
		if a.err != nil {
			t.Fatalf("third accept after release: %v", a.err)
		}
		_ = a.c.Close()
	case <-time.After(time.Second):
		t.Fatal("third accept did not proceed after a slot freed")
	}
	_ = accepted[1].Close()
}

// TestListenerCloseDelegatesToInner is the drain-contract guard: the
// wrapper overrides only Accept, so closing the wrapped Listener (what
// http.Server.Shutdown / go-imap Shutdown / gosmtp gracefulDrain do to
// stop accepting) must close the inner listener. If a future change added
// a Close override that forgot to delegate, the IMAP/CalDAV/submission
// drains would hang on a listener that never actually closed.
func TestListenerCloseDelegatesToInner(t *testing.T) {
	inner, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	ll := New(inner, 8, nil, nil)
	addr := inner.Addr().String()

	if err := ll.Close(); err != nil {
		t.Fatalf("close wrapped listener: %v", err)
	}
	// The inner socket must be closed: a dial to the bound address now
	// fails (no listener) and a direct inner.Accept returns immediately.
	if _, err := inner.Accept(); err == nil {
		t.Fatal("inner.Accept succeeded after wrapped Close — inner not closed")
	}
	if c, derr := net.DialTimeout("tcp", addr, 200*time.Millisecond); derr == nil {
		_ = c.Close()
		t.Fatal("dial to closed listener succeeded — inner socket still open")
	}
}
