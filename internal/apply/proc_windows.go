//go:build windows

package apply

import (
	"os/exec"
	"syscall"
)

// processGroup leaves the run in the process group it is started in: Windows has
// no process groups to kill, and an apply only runs on Linux anyway.
func processGroup() *syscall.SysProcAttr { return nil }

// killGroup kills the run.
func killGroup(command *exec.Cmd) error {
	if command.Process == nil {
		return nil
	}
	return command.Process.Kill()
}
