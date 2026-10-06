package refreshsched

import (
	"testing"
	"time"
)

// schedule mirrors the production backoff used by the tls and capability
// refresh loops: 1m, 5m, then 15m capped.
var schedule = []time.Duration{
	1 * time.Minute,
	5 * time.Minute,
	15 * time.Minute,
}

const interval = 12 * time.Hour

// TestNextDelaySuccessUsesInterval: retryAttempt 0 (just started, or the
// previous refresh succeeded) sleeps the steady-state interval.
func TestNextDelaySuccessUsesInterval(t *testing.T) {
	t.Parallel()
	if got := NextDelay(0, interval, schedule); got != interval {
		t.Fatalf("NextDelay(0) = %v, want %v", got, interval)
	}
}

// TestNextDelayFailureUsesBackoffCapped: consecutive failures walk the
// backoff table and stay pinned at the last (longest) entry.
func TestNextDelayFailureUsesBackoffCapped(t *testing.T) {
	t.Parallel()
	cases := []struct {
		attempt int
		want    time.Duration
	}{
		{1, 1 * time.Minute},
		{2, 5 * time.Minute},
		{3, 15 * time.Minute},
		{4, 15 * time.Minute},
		{50, 15 * time.Minute},
	}
	for _, c := range cases {
		if got := NextDelay(c.attempt, interval, schedule); got != c.want {
			t.Fatalf("NextDelay(%d) = %v, want %v", c.attempt, got, c.want)
		}
	}
}

// TestNextDelayNeverZeroOnFailure is the load-bearing regression guard
// for the example.com disk-fill incident (2026-06-01): the previous
// scheduler interleaved a "next scheduled tick" deadline that, once it
// fell into the past during a long failure streak, collapsed the sleep
// to zero and span the loop at full CPU — a single mail-bridge json log
// grew to 69 GB and filled the disk. A delay of zero on a persistent
// failure must be impossible for any attempt count.
func TestNextDelayNeverZeroOnFailure(t *testing.T) {
	t.Parallel()
	for attempt := 1; attempt <= 10000; attempt++ {
		if got := NextDelay(attempt, interval, schedule); got <= 0 {
			t.Fatalf("NextDelay(%d) = %v; must be > 0 (busy-spin regression)", attempt, got)
		}
	}
}

// TestNextDelayEmptyScheduleFallsBackToInterval: defensive — a caller
// that passes no backoff table still gets a positive delay, never zero.
func TestNextDelayEmptyScheduleFallsBackToInterval(t *testing.T) {
	t.Parallel()
	if got := NextDelay(3, interval, nil); got != interval {
		t.Fatalf("NextDelay(3, nil schedule) = %v, want %v", got, interval)
	}
}
