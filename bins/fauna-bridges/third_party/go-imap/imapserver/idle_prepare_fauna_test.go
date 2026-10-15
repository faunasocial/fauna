// FAUNA-FORK: wire-level tests for the additive IDLE-prepare seam (see
// FORK.md row 20). They prove handleIdle runs a session's optional
// PrepareIdle BEFORE it writes the `+ idling` continuation — so whatever the
// session registers there (the MDA's push subscription) is live by the time
// the client can act on the continuation — and that a failed PrepareIdle
// refuses the IDLE with a tagged response and never writes `+` nor enters
// Session.Idle. Causal, not timed: the continuation is observed, then the
// session's record of what had happened by then is read.
package imapserver_test

import (
	"strings"
	"sync/atomic"
	"testing"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapserver"
)

// prepareIdleSession is a minimal PreAuth session with the optional
// PrepareIdle hook. prepared is set inside PrepareIdle; idleSawPrepared
// records, at Idle entry, whether PrepareIdle had already run.
type prepareIdleSession struct {
	imapserver.SessionIMAP4rev2
	prepareErr      error
	prepared        atomic.Bool
	idleCalled      atomic.Bool
	idleSawPrepared atomic.Bool
}

func (*prepareIdleSession) Close() error                              { return nil }
func (*prepareIdleSession) Login(_, _ string) error                   { return imapserver.ErrAuthFailed }
func (*prepareIdleSession) Poll(*imapserver.UpdateWriter, bool) error { return nil }

func (s *prepareIdleSession) PrepareIdle() error {
	if s.prepareErr != nil {
		return s.prepareErr
	}
	s.prepared.Store(true)
	return nil
}

func (s *prepareIdleSession) Idle(_ *imapserver.UpdateWriter, stop <-chan struct{}) error {
	s.idleSawPrepared.Store(s.prepared.Load())
	s.idleCalled.Store(true)
	<-stop
	return nil
}

// TestForkIdlePrepareRunsBeforeContinuation: by the time the client reads
// `+ idling`, PrepareIdle has already returned.
func TestForkIdlePrepareRunsBeforeContinuation(t *testing.T) {
	sess := &prepareIdleSession{}
	_, conn, r := startShutdownServer(t, true, false, func() imapserver.Session { return sess })
	if _, err := r.ReadString('\n'); err != nil {
		t.Fatalf("read greeting: %v", err)
	}

	if _, err := conn.Write([]byte("a IDLE\r\n")); err != nil {
		t.Fatalf("write IDLE: %v", err)
	}
	line, err := r.ReadString('\n')
	if err != nil {
		t.Fatalf("read continuation: %v", err)
	}
	if !strings.HasPrefix(line, "+") {
		t.Fatalf("want `+ idling` continuation, got %q", line)
	}
	if !sess.prepared.Load() {
		t.Fatal("the `+ idling` continuation was written before PrepareIdle ran — " +
			"a change the client makes on seeing `+` can miss what PrepareIdle registers")
	}

	if _, err := conn.Write([]byte("DONE\r\n")); err != nil {
		t.Fatalf("write DONE: %v", err)
	}
	line, err = r.ReadString('\n')
	if err != nil {
		t.Fatalf("read IDLE completion: %v", err)
	}
	if !strings.HasPrefix(line, "a OK") {
		t.Fatalf("want `a OK` after DONE, got %q", line)
	}
	if !sess.idleSawPrepared.Load() {
		t.Error("Session.Idle ran without PrepareIdle having run first")
	}
}

// TestForkIdlePrepareFailureRefusesIdle: a failing PrepareIdle answers the
// IDLE with a tagged NO and no continuation, and Session.Idle never runs.
func TestForkIdlePrepareFailureRefusesIdle(t *testing.T) {
	sess := &prepareIdleSession{prepareErr: &imap.Error{
		Type: imap.StatusResponseTypeNo,
		Text: "push subscription unavailable",
	}}
	_, conn, r := startShutdownServer(t, true, false, func() imapserver.Session { return sess })
	if _, err := r.ReadString('\n'); err != nil {
		t.Fatalf("read greeting: %v", err)
	}

	if _, err := conn.Write([]byte("a IDLE\r\n")); err != nil {
		t.Fatalf("write IDLE: %v", err)
	}
	line, err := r.ReadString('\n')
	if err != nil {
		t.Fatalf("read IDLE answer: %v", err)
	}
	if !strings.HasPrefix(line, "a NO") {
		t.Fatalf("want tagged `a NO` for a failed PrepareIdle, got %q", line)
	}
	if sess.idleCalled.Load() {
		t.Error("Session.Idle ran although PrepareIdle failed")
	}
}
