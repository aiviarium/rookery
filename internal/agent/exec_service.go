package agent

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os/exec"
	"sync"
	"syscall"
	"time"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
)

// execService serves ExecService: one command, streamed.
type execService struct {
	server *Server
}

func (h *execService) Run(ctx context.Context, request *connect.Request[hostv1.ExecServiceRunRequest], stream *connect.ServerStream[hostv1.ExecServiceRunResponse]) error {
	caller, err := callerOf(ctx)
	if err != nil {
		return err
	}
	if caller.role != roleRoot {
		return denied("agent: exec runs only for the server, not for local programs")
	}

	acct, _, err := accountFor(ctx, request.Msg.GetUser().GetName())
	if err != nil {
		return err
	}
	credential, err := acct.credential()
	if err != nil {
		return toConnect(err)
	}

	command := request.Msg.GetCommand()
	if command == "" {
		return invalid("agent: exec needs a command")
	}
	dir := request.Msg.GetDir()
	if dir == "" {
		dir = acct.homeDir()
	}

	env := make(map[string]string)
	if request.Msg.GetDesktop() {
		desktop, err := readEnvFile(h.server.config.DesktopEnvPath)
		if err != nil {
			return connect.NewError(connect.CodeFailedPrecondition, err)
		}
		env = desktop
	}
	for name, value := range request.Msg.GetEnv() {
		env[name] = value
	}

	runCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	if timeout := request.Msg.GetTimeoutSeconds(); timeout > 0 {
		var stop context.CancelFunc
		runCtx, stop = context.WithTimeout(runCtx, time.Duration(timeout)*time.Second)
		defer stop()
	}

	cmd := exec.CommandContext(runCtx, command, request.Msg.GetArgs()...) //nolint:gosec // the caller names the command on purpose.
	cmd.Dir = dir
	cmd.Env = acct.environ(env)
	cmd.SysProcAttr = &syscall.SysProcAttr{Credential: credential, Setpgid: true}
	// The command runs in its own process group, so a cancel reaches everything
	// it started, not only the first process.
	cmd.Cancel = func() error {
		if cmd.Process == nil {
			return nil
		}
		return syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
	}
	cmd.WaitDelay = 5 * time.Second
	if stdin := request.Msg.GetStdin(); len(stdin) > 0 {
		cmd.Stdin = bytes.NewReader(stdin)
	}

	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return toConnect(err)
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return toConnect(err)
	}
	if err := cmd.Start(); err != nil {
		return toConnect(translateStartError(err))
	}

	// The two pipes are read by two goroutines and multiplexed onto one channel,
	// because a stream has one sender.
	events := make(chan *hostv1.ExecServiceRunResponse, 32)
	var readers sync.WaitGroup
	readers.Add(2)
	go readOutput(stdout, false, events, runCtx.Done(), &readers)
	go readOutput(stderr, true, events, runCtx.Done(), &readers)
	go func() {
		readers.Wait()
		close(events)
	}()

	var sendErr error
	for event := range events {
		if sendErr = stream.Send(event); sendErr != nil {
			// The caller is gone; stop the command and let the readers finish so
			// that Wait does not close a pipe under them.
			cancel()
			_ = cmd.Process.Kill()
			for range events { //nolint:revive // draining so the readers can end.
			}
			break
		}
	}

	waitErr := cmd.Wait()
	if sendErr != nil {
		return sendErr
	}
	if errors.Is(runCtx.Err(), context.DeadlineExceeded) {
		return connect.NewError(connect.CodeDeadlineExceeded, fmt.Errorf(
			"agent: %s ran longer than %s", command, time.Duration(request.Msg.GetTimeoutSeconds())*time.Second))
	}
	if ctx.Err() != nil {
		return connect.NewError(connect.CodeCanceled, ctx.Err())
	}
	return stream.Send(&hostv1.ExecServiceRunResponse{
		Event: &hostv1.ExecServiceRunResponse_Exit{Exit: &hostv1.Exit{Code: int32(exitCodeOf(waitErr))}}, //nolint:gosec // an exit code is small.
	})
}

// readOutput copies a pipe onto the events channel, keeping stdout and stderr
// apart, until the pipe closes or the call is over.
func readOutput(pipe io.Reader, isStderr bool, events chan<- *hostv1.ExecServiceRunResponse, done <-chan struct{}, readers *sync.WaitGroup) {
	defer readers.Done()

	buffer := make([]byte, chunkSize)
	for {
		read, err := pipe.Read(buffer)
		if read > 0 {
			event := &hostv1.ExecServiceRunResponse{
				Event: &hostv1.ExecServiceRunResponse_Stdout{Stdout: append([]byte(nil), buffer[:read]...)},
			}
			if isStderr {
				event.Event = &hostv1.ExecServiceRunResponse_Stderr{Stderr: append([]byte(nil), buffer[:read]...)}
			}
			select {
			case events <- event:
			case <-done:
				return
			}
		}
		if err != nil {
			return
		}
	}
}
