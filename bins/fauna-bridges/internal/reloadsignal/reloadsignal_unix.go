//go:build !windows

package reloadsignal

import (
	"os"
	"os/signal"
	"syscall"
)

// Notify registers ch to receive the reload-poke signal (SIGHUP). The
// caller owns ch and is responsible for signal.Stop when done.
func Notify(ch chan<- os.Signal) {
	signal.Notify(ch, syscall.SIGHUP)
}
