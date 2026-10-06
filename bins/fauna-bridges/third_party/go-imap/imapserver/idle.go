package imapserver

import (
	"errors"
	"fmt"
	"io"
	"net"
	"runtime/debug"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/internal/imapwire"
)

func (c *Conn) handleIdle(dec *imapwire.Decoder) error {
	if !dec.ExpectCRLF() {
		return dec.Err()
	}

	if err := c.checkState(imap.ConnStateAuthenticated); err != nil {
		return err
	}

	if err := c.writeContReq("idling"); err != nil {
		return err
	}

	stop := make(chan struct{})
	done := make(chan error, 1)
	go func() {
		defer func() {
			if v := recover(); v != nil {
				c.server.logger().Printf("panic idling: %v\n%s", v, debug.Stack())
				done <- fmt.Errorf("imapserver: panic idling")
			}
		}()
		w := &UpdateWriter{conn: c, allowExpunge: true}
		done <- c.session.Idle(w, stop)
	}()

	// FAUNA-FORK: the IDLE read deadline is the per-server idle timeout
	// (RFC 2177 §3 — the server may end an inactive IDLE), sourced from the
	// session's configured value so an admin's hot-reloaded
	// imap.idle_timeout_seconds binds on the next IDLE (mail-policy-config.md).
	// Falls back to the 35-min framework default when the session doesn't
	// expose one. The seam-level timer inside Session.Idle returns on the same
	// budget but cannot close the wire connection itself — only this deadline
	// can, by waking the ReadLine below; the two derive from one config value.
	idleTimeout := idleReadTimeout
	if it, ok := c.session.(interface{ IdleTimeout() time.Duration }); ok {
		if d := it.IdleTimeout(); d > 0 {
			idleTimeout = d
		}
	}

	// FAUNA-FORK: an IDLE'ing connection is parked waiting on the client; it
	// is not draining in-flight work, so clear inFlight for the duration of
	// the wait. That makes a graceful Server.Shutdown wake it (read-deadline
	// poke) and BYE it promptly instead of holding the full grace window.
	c.inFlight.Store(false)
	c.setReadTimeout(idleTimeout)
	line, isPrefix, err := c.br.ReadLine()
	c.inFlight.Store(true)
	close(stop)
	if err == io.EOF {
		return nil
	} else if err != nil {
		// A read woken by Shutdown's deadline poke while draining → leave with
		// BYE (the serve loop handles errDraining), not a SERVERBUG response.
		if c.server.shuttingDown() {
			return errDraining
		}
		// FAUNA-FORK: a read-deadline timeout while NOT shutting down is the
		// per-server IDLE timeout firing (the client never sent DONE within the
		// budget). End the inactive IDLE the RFC 2177 §3 way — `* BYE` + close —
		// and signal the serve loop to break quietly (it already happened here);
		// errIdleServerTimeout is handed straight back like errDraining.
		var ne net.Error
		if errors.As(err, &ne) && ne.Timeout() {
			_ = c.Bye("Idle timeout; closing connection")
			return errIdleServerTimeout
		}
		return err
	} else if isPrefix || string(line) != "DONE" {
		return newClientBugError("Syntax error: expected DONE to end IDLE command")
	}

	return <-done
}
