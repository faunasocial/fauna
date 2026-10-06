// Package refreshsched computes the sleep delay for the mail-bridge's
// background refresh loops (TLS certificate and capability grants). It
// exists to share one correct, busy-spin-proof scheduling policy between the
// tls and capability packages, whose refresh loops are otherwise identical.
package refreshsched

import "time"

// NextDelay returns how long a refresh loop should sleep before its next
// attempt.
//
//   - retryAttempt <= 0 means the previous attempt succeeded (or the loop
//     just started): sleep the steady-state interval.
//   - retryAttempt >= 1 means that many consecutive failures: sleep the
//     backoff entry for that attempt, capped at the last (longest) entry.
//     On persistent failure the loop therefore retries forever at the
//     capped cadence — a few log lines per hour, never a flood.
//
// The returned delay is ALWAYS > 0 as long as interval > 0 and the
// schedule entries are > 0. This is the load-bearing guarantee: an
// earlier implementation interleaved a "next scheduled tick" deadline
// that, once it fell into the past during a long failure streak,
// collapsed the wait to zero and span the loop at full CPU — flooding
// the logs until the disk filled (a single 69 GB mail-bridge json log on
// example.com, 2026-06-01). Keeping the policy a pure function of
// retryAttempt makes that class of bug impossible.
func NextDelay(retryAttempt int, interval time.Duration, schedule []time.Duration) time.Duration {
	if retryAttempt <= 0 || len(schedule) == 0 {
		return interval
	}
	i := retryAttempt - 1
	if i >= len(schedule) {
		i = len(schedule) - 1
	}
	return schedule[i]
}
