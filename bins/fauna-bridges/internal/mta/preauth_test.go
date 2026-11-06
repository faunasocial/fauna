package mta

import (
	"encoding/hex"
	"net"
	"sync"
	"testing"
	"time"

	"github.com/emersion/go-sasl"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/auth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/connlimit"
)

// deadlineConn is a fake accepted socket recording every read and write
// deadline.
type deadlineConn struct {
	net.Conn // nil: only the overridden methods are called
	mu       sync.Mutex
	reads    []time.Time
	writes   []time.Time
}

func (c *deadlineConn) SetReadDeadline(t time.Time) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.reads = append(c.reads, t)
	return nil
}

func (c *deadlineConn) SetWriteDeadline(t time.Time) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.writes = append(c.writes, t)
	return nil
}

func lastOf(ds []time.Time) (time.Time, bool) {
	if len(ds) == 0 {
		return time.Time{}, false
	}
	return ds[len(ds)-1], true
}

func (c *deadlineConn) last() (time.Time, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return lastOf(c.reads)
}

func (c *deadlineConn) lastWrite() (time.Time, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return lastOf(c.writes)
}

type oneConnListener struct {
	net.Listener
	conn net.Conn
}

func (l *oneConnListener) Accept() (net.Conn, error) { return l.conn, nil }

// tlsLike stands in for the *tls.Conn that sits above the wrapper on 465.
type tlsLike struct {
	net.Conn
	inner net.Conn
}

func (w tlsLike) NetConn() net.Conn { return w.inner }

// TestSubmissionAuthSuccessLiftsPreAuthDeadline: the connection's pre-auth
// bound is set at accept on both halves and cleared on the inner socket once
// AUTH PLAIN succeeds, through the TLS layer 465 puts above the wrapper. No
// wall clock.
func TestSubmissionAuthSuccessLiftsPreAuthDeadline(t *testing.T) {
	inner := &deadlineConn{}
	ln := connlimit.NewPreAuthListener(&oneConnListener{conn: inner}, nil)
	accepted, err := ln.Accept()
	if err != nil {
		t.Fatalf("accept: %v", err)
	}
	if got, ok := inner.last(); !ok || got.IsZero() {
		t.Fatalf("accept must set a pre-auth read deadline, got %v (set=%v)", got, ok)
	}
	if got, ok := inner.lastWrite(); !ok || got.IsZero() {
		t.Fatalf("accept must set a pre-auth write deadline, got %v (set=%v)", got, ok)
	}

	caller := &submissionAuthCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID()),
		submissionBlob:            mustReadFixture(t, "wrapped_submission_token.bin"),
	}
	sess := newTestSubmissionSession(caller)
	sess.netConn = tlsLike{inner: accepted}

	payload := auth.BuildPlainPayload("", "alice@example.com", string(fixturePlainPassword))
	if err := sess.authenticate(sasl.Plain, payload); err != nil {
		t.Fatalf("authenticate: %v", err)
	}
	if got, _ := inner.last(); !got.IsZero() {
		t.Fatalf("after AUTH success the inner read deadline = %v, want cleared", got)
	}
	if got, _ := inner.lastWrite(); !got.IsZero() {
		t.Fatalf("after AUTH success the inner write deadline = %v, want cleared", got)
	}
}

// TestSubmissionAuthFailureKeepsPreAuthDeadline: a failed AUTH leaves the
// bound standing.
func TestSubmissionAuthFailureKeepsPreAuthDeadline(t *testing.T) {
	inner := &deadlineConn{}
	ln := connlimit.NewPreAuthListener(&oneConnListener{conn: inner}, nil)
	accepted, err := ln.Accept()
	if err != nil {
		t.Fatalf("accept: %v", err)
	}
	caller := &submissionAuthCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID()),
		submissionBlob:            mustReadFixture(t, "wrapped_submission_token.bin"),
	}
	sess := newTestSubmissionSession(caller)
	sess.netConn = accepted

	payload := auth.BuildPlainPayload("", "alice@example.com", "wrong-password")
	if err := sess.authenticate(sasl.Plain, payload); err == nil {
		t.Fatal("authenticate with a wrong password must fail")
	}
	if got, _ := inner.last(); got.IsZero() {
		t.Fatal("a failed AUTH must not lift the pre-auth deadline")
	}
	if got, _ := inner.lastWrite(); got.IsZero() {
		t.Fatal("a failed AUTH must not lift the pre-auth write deadline")
	}
}
