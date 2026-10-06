package mta

import (
	"context"
	"errors"
	"log/slog"
	"net"
	"net/smtp"
	"strings"
	"testing"
	"time"

	gosmtp "github.com/emersion/go-smtp"
)

// TestInboundSessionMailRejectsWhenDraining asserts the inbound MAIL FROM
// gate answers 421 4.3.2 once the backend is draining (graceful-shutdown
// step 2). A non-draining session is unaffected by the gate (it proceeds
// past it — here with no policy wired, Mail returns nil).
func TestInboundSessionMailRejectsWhenDraining(t *testing.T) {
	d := newDrainTracker()
	s := &inboundSession{logger: slog.Default(), clientIP: "127.0.0.1", drain: d}

	// Not draining yet: the gate is transparent (no policy ⇒ Mail accepts).
	if err := s.Mail("alice@example.org", nil); err != nil {
		t.Fatalf("pre-drain Mail returned %v, want nil", err)
	}

	d.startDraining()
	err := s.Mail("alice@example.org", nil)
	assertShuttingDown421(t, err)
}

// TestSubmissionSessionMailRejectsWhenDraining is the submission-listener
// analog: a draining submission backend answers 421 4.3.2 on MAIL FROM,
// ahead of the auth-required gate.
func TestSubmissionSessionMailRejectsWhenDraining(t *testing.T) {
	d := newDrainTracker()
	b := &submissionBackend{logger: slog.Default(), drain: d}
	s := &submissionSession{backend: b}

	d.startDraining()
	err := s.Mail("alice@example.org", nil)
	assertShuttingDown421(t, err)
}

// TestInboundListenerGracefulDrain drives the full runner path: a
// connection that is still open when SIGTERM (ctx cancel) arrives gets
// 421 4.3.2 on a new MAIL FROM, and once it QUITs the listener returns nil
// (clean drain within grace) — exercising gracefulDrain steps 1–3.
func TestInboundListenerGracefulDrain(t *testing.T) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	drain := newDrainTracker()
	// policy + caller nil ⇒ skeleton path: NewSession/Mail/Rcpt no-op their
	// policy gates, so the only gate exercised is the draining 421.
	backend := &inboundBackend{logger: slog.Default(), drain: drain}

	ctx, cancel := context.WithCancel(context.Background())
	serveErr := make(chan error, 1)
	go func() {
		serveErr <- runListenerWithBackend(ctx, ln, backend, nil, "test.example", 0, 5*time.Second, drain, slog.Default())
	}()
	addr := ln.Addr().String()
	time.Sleep(20 * time.Millisecond) // let Serve start before dialing

	c, err := smtp.Dial(addr)
	if err != nil {
		t.Fatalf("smtp.Dial: %v", err)
	}
	if err := c.Hello("test.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}

	// Begin shutdown and wait until draining is actually in effect, so the
	// MAIL FROM below deterministically races after the flag is set.
	cancel()
	deadline := time.Now().Add(2 * time.Second)
	for !drain.isDraining() {
		if time.Now().After(deadline) {
			t.Fatal("drain did not begin after ctx cancel")
		}
		time.Sleep(2 * time.Millisecond)
	}

	// A new MAIL FROM on the still-open connection must be refused 421.
	if err := c.Mail("alice@example.org"); err == nil {
		t.Fatal("MAIL FROM during drain returned nil, want 421")
	} else if !strings.Contains(err.Error(), "421") {
		t.Fatalf("MAIL FROM during drain: got %q, want a 421", err.Error())
	}

	// QUIT lets the in-flight session leave → the drain completes cleanly.
	_ = c.Quit()

	select {
	case e := <-serveErr:
		if e != nil {
			t.Fatalf("clean drain expected nil, got %v", e)
		}
	case <-time.After(4 * time.Second):
		t.Fatal("listener did not return after clean drain")
	}
}

func assertShuttingDown421(t *testing.T, err error) {
	t.Helper()
	if err == nil {
		t.Fatal("draining Mail returned nil, want 421 4.3.2")
	}
	var smtpErr *gosmtp.SMTPError
	if !errors.As(err, &smtpErr) {
		t.Fatalf("expected *gosmtp.SMTPError, got %T: %v", err, err)
	}
	if smtpErr.Code != 421 || smtpErr.EnhancedCode != (gosmtp.EnhancedCode{4, 3, 2}) {
		t.Fatalf("got %d %v, want 421 [4 3 2]", smtpErr.Code, smtpErr.EnhancedCode)
	}
}

func TestDrainTracker_CleanDrainWithinGrace(t *testing.T) {
	d := newDrainTracker()
	d.enter()
	d.enter()
	if got := d.inFlightCount(); got != 2 {
		t.Fatalf("inFlightCount = %d, want 2", got)
	}

	// Both sessions finish well before the grace window.
	go func() {
		time.Sleep(10 * time.Millisecond)
		d.leave()
		d.leave()
	}()

	if !d.waitInFlight(2 * time.Second) {
		t.Fatalf("waitInFlight = false, want clean drain")
	}
	if got := d.inFlightCount(); got != 0 {
		t.Fatalf("inFlightCount after drain = %d, want 0", got)
	}
}

func TestDrainTracker_ForceCloseOnGraceExpiry(t *testing.T) {
	d := newDrainTracker()
	d.enter()
	d.enter()

	// Sessions never finish within the (tiny) grace window.
	if d.waitInFlight(20 * time.Millisecond) {
		t.Fatalf("waitInFlight = true, want force-close (grace expired)")
	}
	if got := d.inFlightCount(); got != 2 {
		t.Fatalf("pending count at force-close = %d, want 2", got)
	}

	// After force-close, leaving drains the tracker (the cond goroutine
	// from waitInFlight must not be left wedged).
	d.leave()
	d.leave()
	if got := d.inFlightCount(); got != 0 {
		t.Fatalf("inFlightCount after force leave = %d, want 0", got)
	}
}

func TestDrainTracker_IdleDrainsImmediately(t *testing.T) {
	d := newDrainTracker()
	if !d.waitInFlight(time.Second) {
		t.Fatalf("idle waitInFlight = false, want clean (nothing in flight)")
	}
	// Zero grace with nothing in flight is also clean.
	if !d.waitInFlight(0) {
		t.Fatalf("idle waitInFlight(0) = false, want clean")
	}
}

func TestDrainTracker_ZeroGraceWithInflightForces(t *testing.T) {
	d := newDrainTracker()
	d.enter()
	defer d.leave()
	// grace=0 means immediate force-close when anything is still in flight.
	if d.waitInFlight(0) {
		t.Fatalf("waitInFlight(0) with one in flight = true, want force-close")
	}
}

func TestDrainTracker_DrainingFlag(t *testing.T) {
	d := newDrainTracker()
	if d.isDraining() {
		t.Fatalf("fresh tracker reports draining")
	}
	d.startDraining()
	if !d.isDraining() {
		t.Fatalf("startDraining did not set the flag")
	}
}
