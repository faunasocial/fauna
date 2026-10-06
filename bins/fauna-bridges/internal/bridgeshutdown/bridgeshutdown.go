// Package bridgeshutdown holds the cross-role graceful-shutdown sentinel.
//
// Both the MTA (internal/mta) and MDA (internal/mda) role loops drain
// their listeners on SIGTERM up to mail.bridge.shutdown_grace_seconds and,
// when the grace window expires with work still in flight, force-close the
// stragglers and report it. Each role surfaces that outcome by returning
// ErrShutdownForced from its Run; main.go maps the sentinel to process exit
// code 1 (per docs/goal/behavior/mail-bridge-lifecycle.md § Shutting down
// step 6) WITHOUT a stderr crash dump — it is an expected, logged outcome of
// a busy bridge being told to stop, not a panic.
//
// The sentinel lives in its own tiny package so the two role packages and
// main share exactly one concept with one home,
// rather than one role re-exporting the other's symbol.
package bridgeshutdown

import "errors"

// ErrShutdownForced is returned by a role's Run when its graceful-shutdown
// grace window expired with transactions still in flight, so a listener had
// to force-close them.
var ErrShutdownForced = errors.New("mail-bridge: graceful shutdown grace window expired; force-closed in-flight connections")
