// Package reloadsignal abstracts the OS reload-poke signal used by the
// mail-bridge's cert (internal/tls) and capability-grant
// (internal/capability) refresh loops.
//
// On Unix the bridge treats SIGHUP as an ops-poke that triggers an
// immediate refresh; on Windows there is no SIGHUP, so Notify is a
// no-op there. In both cases the refresh is fundamentally timer-driven
// (and, on the desktop box, re-fetched whenever the Rust bridge-service
// shell restarts the child), so the signal is a latency optimization,
// never a correctness requirement. In-process callers and tests use the
// respective registry's TriggerRefresh, which is unaffected by OS.
package reloadsignal
