//go:build !windows

package cli

import (
	"context"
	"io"
	"os"
	"os/signal"
	"syscall"
)

func watchTerminalSize(ctx context.Context, output io.Writer, resize func(uint32, uint32)) func() {
	if cols, rows := terminalSize(output); cols == 0 || rows == 0 {
		return func() {}
	}
	changes := make(chan os.Signal, 1)
	signal.Notify(changes, syscall.SIGWINCH)
	done := make(chan struct{})
	go func() {
		defer close(done)
		for {
			select {
			case <-ctx.Done():
				return
			case <-changes:
				resize(terminalSize(output))
			}
		}
	}()
	return func() {
		signal.Stop(changes)
		<-done
	}
}
