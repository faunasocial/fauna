//go:build windows

package reloadsignal

import "os"

// Notify is a no-op on Windows: there is no SIGHUP. The cert /
// capability refresh loops fall back to their timer cadence (and re-fetch on a
// service restart), so the missing ops-poke costs only refresh latency,
// not correctness. The ch argument is accepted for signature parity.
func Notify(ch chan<- os.Signal) {}
