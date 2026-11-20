//go:build unix

package apply

import (
	"errors"
	"os/exec"
	"syscall"
)

// processGroup puts the run in a process group of its own, so that a cancelled
// apply takes the SSH and Python it started with it.
func processGroup() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{Setpgid: true}
}

// killGroup kills the run's process group.
func killGroup(command *exec.Cmd) error {
	if command.Process == nil {
		return nil
	}
	err := syscall.Kill(-command.Process.Pid, syscall.SIGKILL)
	if errors.Is(err, syscall.ESRCH) {
		return nil // it has exited already
	}
	return err
}
