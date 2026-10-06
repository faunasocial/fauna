//go:build !windows

package main

import (
	"os/exec"
	"syscall"
)

// setProcGroup puts the spawned bridge in its own process group so the
// whole group can be torn down on cleanup (Unix).
func setProcGroup(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
}

// killProcGroup best-effort kills the bridge's process group (Unix).
func killProcGroup(cmd *exec.Cmd) {
	_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
}
