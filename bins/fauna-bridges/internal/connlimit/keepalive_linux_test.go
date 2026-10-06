//go:build linux

package connlimit

import (
	"net"
	"syscall"
	"testing"
	"time"
)

// The Go bridge is the third caller of the per-IP-cap pattern (nest's serve_tls
// and fauna-sni-router are the other two, sharing the Rust libs/fauna-conn-limit
// crate). All three release their permit when the connection closes, so all
// three have the same failure mode: a peer that vanishes without a clean TCP
// close leaves an ESTABLISHED socket forever and burns one of its IP's slots
// permanently. That is what took example.com's :443 down on 2026-07-31, once ~256
// such sockets had accumulated on the router.
//
// The Rust side had to arm TCP keepalive explicitly (SO_KEEPALIVE is off by
// default for a raw socket). The Go side is believed to inherit it from the
// runtime's own listener defaults instead — which is a fine reason NOT to write
// code, but a bad reason to write nothing: "the runtime probably handles it" is
// exactly the kind of unverified assumption that leaves a leak in place while
// looking closed. So assert it, on a connection accepted through the bridge's
// real listener wrapper.
func TestAcceptedConnectionsHaveDeadPeerDetectionArmed(t *testing.T) {
	// Same construction the bridge uses for its published listeners.
	raw, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer raw.Close()

	limiter := NewPerIPLimiter(16)
	ln := NewPerIPListener(raw, limiter, nil)

	dialed, err := net.Dial("tcp", ln.Addr().String())
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer dialed.Close()

	accepted, err := ln.Accept()
	if err != nil {
		t.Fatalf("accept: %v", err)
	}
	defer accepted.Close()

	// Reach the real socket through the wrapper (same package, so the embedded
	// conn is visible). If the wrapper ever stops passing the TCP conn through,
	// this assertion is the thing that notices.
	wrapped, ok := accepted.(*peripConn)
	if !ok {
		t.Fatalf("accepted conn is %T, want *peripConn", accepted)
	}
	tcp, ok := wrapped.Conn.(*net.TCPConn)
	if !ok {
		t.Fatalf("wrapped conn is %T, want *net.TCPConn", wrapped.Conn)
	}

	sys, err := tcp.SyscallConn()
	if err != nil {
		t.Fatalf("SyscallConn: %v", err)
	}
	var keepalive, idleSecs int
	var kaErr, idleErr error
	if err := sys.Control(func(fd uintptr) {
		keepalive, kaErr = syscall.GetsockoptInt(int(fd), syscall.SOL_SOCKET, syscall.SO_KEEPALIVE)
		idleSecs, idleErr = syscall.GetsockoptInt(int(fd), syscall.IPPROTO_TCP, syscall.TCP_KEEPIDLE)
	}); err != nil {
		t.Fatalf("Control: %v", err)
	}
	if kaErr != nil {
		t.Fatalf("getsockopt SO_KEEPALIVE: %v", kaErr)
	}
	if idleErr != nil {
		t.Fatalf("getsockopt TCP_KEEPIDLE: %v", idleErr)
	}

	if keepalive == 0 {
		t.Fatal("SO_KEEPALIVE is off on an accepted connection: a peer that vanishes " +
			"without a clean close would hold its per-IP permit forever (the 2026-07-31 " +
			"example.com :443 outage class). Arm it explicitly on the listener, the way " +
			"libs/fauna-conn-limit::arm_dead_peer_detection does for the Rust callers.")
	}
	// The OS default idle is ~2h, which is too slow to keep a 256-slot budget
	// healthy. Anything at or under the Rust side's 120s ceiling is fine; the
	// bound is what matters, not the exact value.
	const maxIdle = 120
	if idleSecs > maxIdle {
		t.Fatalf("TCP_KEEPIDLE is %ds, want <= %ds — probing this lazily lets dead peers "+
			"hold per-IP permits for hours", idleSecs, maxIdle)
	}
	t.Logf("accepted conn: SO_KEEPALIVE=on, TCP_KEEPIDLE=%s", time.Duration(idleSecs)*time.Second)
}
