// The lockout's own invariant tests moved with the implementation to
// `internal/authlock/lockout_test.go`.
// This file keeps the deterministic test clock the submission AUTH integration
// tests (`auth_test.go`) use to drive the lockout without sleeping.
package mta

import (
	"sync"
	"time"
)

// fakeLockoutClock is a goroutine-safe deterministic clock. It satisfies both
// the MTA's Clock and authlock.Clock (same method set).
type fakeLockoutClock struct {
	mu  sync.Mutex
	now time.Time
}

func (c *fakeLockoutClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *fakeLockoutClock) advance(d time.Duration) {
	c.mu.Lock()
	c.now = c.now.Add(d)
	c.mu.Unlock()
}

func newFakeLockoutClock() *fakeLockoutClock {
	return &fakeLockoutClock{now: time.Unix(1_700_000_000, 0)}
}
