// FAUNA-FORK: wire-level tests for the additive graceful-shutdown seam
// (see FORK.md row 18). They prove Server.Shutdown sends an untagged
// `* BYE` to a parked/idle connection and drains it cleanly (forced=false),
// and that a connection executing a command when Shutdown fires is NOT
// interrupted but force-closed once the grace window expires (forced=true,
// pending counted). A minimal Session stands in for the MDA so the test
// depends on no fauna crates. The shared `command` / `discardLogger`
// helpers live in condstore_fauna_test.go (same package).
package imapserver_test

import (
	"bufio"
	"context"
	"net"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapserver"
)

// idleShutdownSession is a minimal PreAuth Session: an idle connection only
// ever has Close invoked on it during graceful teardown.
type idleShutdownSession struct {
	imapserver.SessionIMAP4rev2
}

func (idleShutdownSession) Close() error                              { return nil }
func (idleShutdownSession) Login(_, _ string) error                   { return imapserver.ErrAuthFailed }
func (idleShutdownSession) Poll(*imapserver.UpdateWriter, bool) error { return nil }

// blockingLoginSession blocks inside Login until release is closed, so a test
// can hold a connection "in flight" (mid-command) across a Shutdown call.
type blockingLoginSession struct {
	imapserver.SessionIMAP4rev2
	entered sync.Once
	enterCh chan struct{}
	release chan struct{}
}

func (blockingLoginSession) Close() error                              { return nil }
func (blockingLoginSession) Poll(*imapserver.UpdateWriter, bool) error { return nil }
func (s *blockingLoginSession) Login(_, _ string) error {
	s.entered.Do(func() { close(s.enterCh) })
	<-s.release
	return imapserver.ErrAuthFailed
}

// startShutdownServer spins a loopback server with the given session factory
// and returns the server (so the test can call Shutdown), a connected raw
// client, and its buffered reader. preAuth controls the greeting state;
// insecureAuth allows LOGIN without TLS (needed by the in-flight test).
func startShutdownServer(t *testing.T, preAuth, insecureAuth bool, newSession func() imapserver.Session) (*imapserver.Server, net.Conn, *bufio.Reader) {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	srv := imapserver.New(&imapserver.Options{
		NewSession: func(*imapserver.Conn) (imapserver.Session, *imapserver.GreetingData, error) {
			return newSession(), &imapserver.GreetingData{PreAuth: preAuth}, nil
		},
		Caps:         imap.CapSet{imap.CapIMAP4rev2: {}},
		Logger:       discardLogger{},
		InsecureAuth: insecureAuth,
	})
	go func() { _ = srv.Serve(ln) }()
	t.Cleanup(func() { _ = srv.Close(); _ = ln.Close() })

	conn, err := net.Dial("tcp", ln.Addr().String())
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	_ = conn.SetDeadline(time.Now().Add(10 * time.Second))
	return srv, conn, bufio.NewReader(conn)
}

// TestForkShutdownBYEsIdleAndDrainsClean: a connection parked between commands
// is woken by Shutdown, receives `* BYE`, and the drain completes cleanly
// (forced=false) — the prompt-eviction property this fork's Shutdown
// implementation calls for (don't hold the full grace for idle/IDLE).
func TestForkShutdownBYEsIdleAndDrainsClean(t *testing.T) {
	srv, _, r := startShutdownServer(t, true, false, func() imapserver.Session {
		return idleShutdownSession{}
	})

	// Read the greeting first: by the time it lands the connection is fully
	// registered and the command loop is parked in its tag read.
	if _, err := r.ReadString('\n'); err != nil {
		t.Fatalf("read greeting: %v", err)
	}

	type result struct {
		forced  bool
		pending int
	}
	done := make(chan result, 1)
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		forced, pending := srv.Shutdown(ctx)
		done <- result{forced, pending}
	}()

	// The parked connection is woken and BYE'd.
	line, err := r.ReadString('\n')
	if err != nil {
		t.Fatalf("read after Shutdown: %v", err)
	}
	if !strings.HasPrefix(line, "* BYE") {
		t.Errorf("want untagged `* BYE` on graceful shutdown, got %q", line)
	}

	select {
	case res := <-done:
		if res.forced {
			t.Errorf("idle drain should be clean (forced=false), got forced=true pending=%d", res.pending)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Shutdown did not return after the idle connection left")
	}
}

// TestForkShutdownBYEsNewCommandAfterDraining: once draining has begun, a
// brand-new command on a still-open connection is met with `* BYE` rather than
// being processed (goal-doc § Shutting down step 2, generalised to any command).
func TestForkShutdownBYEsNewCommandAfterDraining(t *testing.T) {
	srv, conn, r := startShutdownServer(t, true, false, func() imapserver.Session {
		return idleShutdownSession{}
	})
	if _, err := r.ReadString('\n'); err != nil {
		t.Fatalf("read greeting: %v", err)
	}

	// Drain begins; the parked connection is BYE'd by the poke. We then prove
	// the connection is gone (a write + read sees the BYE / EOF), i.e. no new
	// command is accepted.
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_, _ = srv.Shutdown(ctx)
	}()

	line, err := r.ReadString('\n')
	if err != nil {
		t.Fatalf("read after Shutdown: %v", err)
	}
	if !strings.HasPrefix(line, "* BYE") {
		t.Fatalf("want `* BYE`, got %q", line)
	}
	// A NOOP after BYE must not get a tagged OK — the connection is closing.
	_, _ = conn.Write([]byte("z1 NOOP\r\n"))
	for {
		l, err := r.ReadString('\n')
		if err != nil {
			break // EOF: connection closed, as expected
		}
		if strings.HasPrefix(l, "z1 ") {
			t.Fatalf("server processed a command after BYE: %q", l)
		}
	}
}

// TestForkShutdownForcesInFlightOnGraceExpiry: a connection executing a command
// when Shutdown fires is NOT interrupted (drained, like an in-flight SMTP DATA);
// when the grace window expires with it still running, Shutdown force-closes it
// and reports forced=true with the pending count (goal-doc step 4 / step 6).
func TestForkShutdownForcesInFlightOnGraceExpiry(t *testing.T) {
	sess := &blockingLoginSession{enterCh: make(chan struct{}), release: make(chan struct{})}
	srv, conn, r := startShutdownServer(t, false, true, func() imapserver.Session { return sess })
	if _, err := r.ReadString('\n'); err != nil {
		t.Fatalf("read greeting: %v", err)
	}

	// Drive the connection into the blocking Login handler (inFlight=true).
	if _, err := conn.Write([]byte("a1 LOGIN user pass\r\n")); err != nil {
		t.Fatalf("write LOGIN: %v", err)
	}
	select {
	case <-sess.enterCh:
	case <-time.After(5 * time.Second):
		t.Fatal("Login handler never entered")
	}

	// Tiny grace: the in-flight handler will not finish, so Shutdown must
	// force-close it after the window and report it pending.
	ctx, cancel := context.WithTimeout(context.Background(), 80*time.Millisecond)
	defer cancel()
	forced, pending := srv.Shutdown(ctx)
	if !forced {
		t.Errorf("in-flight command past grace should force-close (forced=true), got false")
	}
	if pending != 1 {
		t.Errorf("pending count at force-close = %d, want 1", pending)
	}

	// Unblock the handler so its goroutine unwinds (it writes to the now-closed
	// conn, errs, and the session is torn down).
	close(sess.release)
}
