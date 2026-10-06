//go:build windows

package main

import "os/exec"

// setProcGroup is a no-op on Windows: there are no POSIX process groups.
func setProcGroup(cmd *exec.Cmd) {}

// killProcGroup best-effort kills the spawned bridge on Windows. The MDA
// spawns no children of its own, so a single-process kill suffices for
// cleanup. (The graceful-stop path the integration test exercises via
// SIGTERM still needs a Windows console-ctrl equivalent — Track 3.)
func killProcGroup(cmd *exec.Cmd) {
	_ = cmd.Process.Kill()
}
