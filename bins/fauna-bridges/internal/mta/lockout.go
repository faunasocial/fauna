// The per-(credential, source-IP) AUTH-failure lockout implementation was
// lifted to the shared `internal/authlock` package so the MDA's IMAP/CalDAV auth paths get the same online-guessing brake
// the submission path already had, without the MDA importing the whole MTA
// package. These thin shims keep the MTA's existing call sites and tests
// reading unchanged. See `internal/authlock/lockout.go` for the implementation,
// invariants, and the catalog row (`mail.auth.max_auth_failures_per_minute`).
package mta

import (
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/authlock"
)

// AuthLockout is the shared fixed-window AUTH-failure counter.
type AuthLockout = authlock.Lockout

// NewAuthLockout constructs a shared lockout. The MTA's Clock satisfies
// authlock.Clock structurally, so it passes straight through.
func NewAuthLockout(limit uint32, window time.Duration, clk Clock) *authlock.Lockout {
	return authlock.New(limit, window, clk)
}
