package mta

import (
	"errors"
	"log/slog"
	"net"
	"sync"
	"sync/atomic"
	"time"

	gosmtp "github.com/emersion/go-smtp"
)

// The graceful-shutdown sentinel both roles return on a forced drain lives in
// internal/bridgeshutdown.ErrShutdownForced — server.go / submission.go return
// it, mta.Run propagates it, and main.go maps it to exit code 1.

// drainTracker coordinates one SMTP listener's graceful shutdown. It tracks
// the set of in-flight sessions (one per accepted connection) and a
// "draining" flag that, once set, makes the per-session Mail() answer
// 421 4.3.2 on any new MAIL FROM (goal-doc § Shutting down step 2).
//
// One tracker per backend: port 25 (inbound) and 465/587 (submission) have
// independent in-flight sets, and the force/clean result is OR'd across them
// in mta.Run.
//
// A plain sync.WaitGroup is unsafe here: go-smtp's accept loop spawns the
// connection goroutine before our enter() (which runs inside NewSession), so
// a WaitGroup could see Add race Wait at zero and panic. We use a
// mutex-guarded counter + sync.Cond instead, which tolerates enter() landing
// concurrently with waitInFlight.
type drainTracker struct {
	mu       sync.Mutex
	cond     *sync.Cond
	count    int
	draining atomic.Bool
}

func newDrainTracker() *drainTracker {
	d := &drainTracker{}
	d.cond = sync.NewCond(&d.mu)
	return d
}

// enter registers one in-flight session. Pair with exactly one leave().
func (d *drainTracker) enter() {
	d.mu.Lock()
	d.count++
	d.mu.Unlock()
}

// leave deregisters one in-flight session.
func (d *drainTracker) leave() {
	d.mu.Lock()
	d.count--
	if d.count <= 0 {
		d.cond.Broadcast()
	}
	d.mu.Unlock()
}

// startDraining flips the flag so new MAIL FROM answers 421 4.3.2.
func (d *drainTracker) startDraining() { d.draining.Store(true) }

// isDraining reports whether shutdown has begun.
func (d *drainTracker) isDraining() bool { return d.draining.Load() }

// inFlightCount snapshots the current in-flight session count — used for the
// force-close log line.
func (d *drainTracker) inFlightCount() int {
	d.mu.Lock()
	defer d.mu.Unlock()
	return d.count
}

// waitInFlight blocks until the in-flight count reaches zero or grace
// elapses. Returns true if the drain completed cleanly within grace, false
// on timeout (the caller then force-closes). A non-positive grace means "no
// drain window": return immediately, clean only if already idle.
func (d *drainTracker) waitInFlight(grace time.Duration) bool {
	if grace <= 0 {
		d.mu.Lock()
		idle := d.count == 0
		d.mu.Unlock()
		return idle
	}
	done := make(chan struct{})
	go func() {
		d.mu.Lock()
		for d.count > 0 {
			d.cond.Wait()
		}
		d.mu.Unlock()
		close(done)
	}()
	t := time.NewTimer(grace)
	defer t.Stop()
	select {
	case <-done:
		return true
	case <-t.C:
		// The timer and the final leave() can race; re-check so a drain
		// that finished in the same instant still reports clean.
		d.mu.Lock()
		idle := d.count == 0
		d.mu.Unlock()
		return idle
	}
}

// gracefulDrain runs the SIGTERM drain sequence for one go-smtp listener,
// per docs/goal/behavior/mail-bridge-lifecycle.md § Shutting down:
//
//  1. flip the draining flag (new MAIL FROM → 421 4.3.2);
//  2. close the listener so no new connections are accepted (the Serve
//     goroutine returns) WITHOUT force-closing in-flight connections;
//  3. wait up to grace for in-flight sessions to finish;
//  4. on grace expiry, log bridge_shutdown_force_close with the pending
//     count, then srv.Close() to force-close the stragglers.
//
// serveLn is the listener Serve is accepting on (the limit-wrapped listener
// for inbound, the raw listener for submission); serveErrCh receives Serve's
// return value. Returns true iff it had to force-close.
func gracefulDrain(
	srv *gosmtp.Server,
	serveLn net.Listener,
	drain *drainTracker,
	grace time.Duration,
	serveErrCh <-chan error,
	logger *slog.Logger,
) bool {
	if drain != nil {
		drain.startDraining()
	}
	// Stop accepting (step 1). Closing the listener makes go-smtp's Serve
	// loop return on its next Accept; already-accepted connections keep
	// running until they finish or we force them below.
	if err := serveLn.Close(); err != nil && !errors.Is(err, net.ErrClosed) {
		logger.Warn("listener close error (continuing shutdown)", "err", err)
	}
	<-serveErrCh

	clean := true
	if drain != nil {
		clean = drain.waitInFlight(grace) // step 3
		if !clean {
			// step 4: structured force-close line (goal-doc step 4).
			logger.Warn("bridge_shutdown_force_close", "pending_count", drain.inFlightCount())
		}
	}
	// Force-close any connections still open. No-op when the drain was
	// clean (no conns left); on timeout this severs the stragglers.
	if err := srv.Close(); err != nil &&
		!errors.Is(err, net.ErrClosed) &&
		!errors.Is(err, gosmtp.ErrServerClosed) {
		logger.Warn("smtp server close error (continuing shutdown)", "err", err)
	}
	logger.Info("smtp listener stopped", "drained_clean", clean)
	return !clean
}
