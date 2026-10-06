package logplane

import "math"

// Test-only handles on the package's process-global state. They live here
// rather than in logplane.go so production code cannot reach them: the queue's
// whole contract is that nothing outside a flush drains it, and a Reset
// exported for real would be a way to lose an admin's events.

// Reset returns the plane to its start-of-process state.
func Reset() {
	mu.Lock()
	events = nil
	dropped = 0
	mu.Unlock()
	disabled.Store(false)
	// Drain a pending size-trigger nudge so it cannot leak into the next test.
	select {
	case <-nudge:
	default:
	}
}

// setDisabled forces the old-nest state without needing a failing caller.
func setDisabled(v bool) { disabled.Store(v) }

// discardEventsKeepingDrops clears the queued events but leaves the
// accumulated drop count, so the drop-only-batch case can be driven directly.
func discardEventsKeepingDrops() {
	mu.Lock()
	events = nil
	mu.Unlock()
}

// emitEveryCatalogueEventWorstCase drives every catalogue function once, with
// the largest interpolations each accepts and — wherever a call site could be
// tempted to pass one through — a string carrying an address, an actor id, an
// upstream SMTP error and newlines. The catalogue tests then assert on what
// actually queued, so the privacy and size properties are measured on rendered
// output rather than argued from reading the templates.
func emitEveryCatalogueEventWorstCase() {
	const hostile = "victim@example.com did:plc:xyz actor\r\n550 rejected"
	Ready(hostile)
	TLSCertFetchFailed()
	ListenerBindFailed(hostile, 65535)
	NestReconnected(math.MaxInt64)
	ShutdownForced()
	ConfinementDegraded(hostile, hostile)
}
