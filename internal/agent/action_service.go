package agent

import (
	"context"
	"fmt"
	"os/exec"
	"sync"
	"syscall"
	"time"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/hostaction"
)

// actionService serves ActionService: the user actions the apply wrote.
type actionService struct {
	server *Server
}

func (h *actionService) List(ctx context.Context, _ *connect.Request[hostv1.ActionServiceListRequest]) (*connect.Response[hostv1.ActionServiceListResponse], error) {
	if _, err := callerOf(ctx); err != nil {
		return nil, err
	}

	actions, err := h.server.actions.list()
	if err != nil {
		return nil, toConnect(err)
	}
	response := &hostv1.ActionServiceListResponse{Actions: make([]*hostv1.Action, 0, len(actions))}
	for _, action := range actions {
		response.Actions = append(response.Actions, action.proto())
	}
	return connect.NewResponse(response), nil
}

func (h *actionService) Run(ctx context.Context, request *connect.Request[hostv1.ActionServiceRunRequest], stream *connect.ServerStream[hostv1.ActionServiceRunResponse]) error {
	caller, err := callerOf(ctx)
	if err != nil {
		return err
	}

	action, err := h.server.actions.get(request.Msg.GetName())
	if err != nil {
		return toConnect(err)
	}
	acct, err := h.runAs(caller, action, request.Msg.GetUser().GetName())
	if err != nil {
		return err
	}
	credential, err := acct.credential()
	if err != nil {
		return toConnect(err)
	}
	environment, err := action.environment(request.Msg.GetArgsCbor())
	if err != nil {
		return toConnect(err)
	}

	// The action's command is a shell line, as the feature declared it. The
	// arguments reach it through the environment, never through this string.
	cmd := exec.CommandContext(ctx, "/bin/sh", "-c", action.command) //nolint:gosec // the feature's command, on purpose.
	cmd.Dir = acct.homeDir()
	cmd.Env = acct.environ(environment)
	cmd.SysProcAttr = &syscall.SysProcAttr{Credential: credential, Setpgid: true}
	cmd.Cancel = func() error {
		if cmd.Process == nil {
			return nil
		}
		return syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
	}
	cmd.WaitDelay = 5 * time.Second

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
	h.server.log.InfoContext(ctx, "ran an action", "action", action.name, "user", acct.Name)

	events := make(chan *hostv1.ActionServiceRunResponse, 32)
	var readers sync.WaitGroup
	readers.Add(2)
	go readActionOutput(stdout, false, events, ctx.Done(), &readers)
	go readActionOutput(stderr, true, events, ctx.Done(), &readers)
	go func() {
		readers.Wait()
		close(events)
	}()

	var sendErr error
	for event := range events {
		if sendErr = stream.Send(event); sendErr != nil {
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
	if ctx.Err() != nil {
		return connect.NewError(connect.CodeCanceled, ctx.Err())
	}
	return stream.Send(&hostv1.ActionServiceRunResponse{
		Event: &hostv1.ActionServiceRunResponse_Exit{Exit: &hostv1.Exit{Code: int32(exitCodeOf(waitErr))}}, //nolint:gosec // an exit code is small.
	})
}

// readActionOutput copies a pipe onto the events channel.
func readActionOutput(pipe interface{ Read([]byte) (int, error) }, isStderr bool, events chan<- *hostv1.ActionServiceRunResponse, done <-chan struct{}, readers *sync.WaitGroup) {
	defer readers.Done()

	buffer := make([]byte, chunkSize)
	for {
		read, err := pipe.Read(buffer)
		if read > 0 {
			event := &hostv1.ActionServiceRunResponse{
				Event: &hostv1.ActionServiceRunResponse_Stdout{Stdout: append([]byte(nil), buffer[:read]...)},
			}
			if isStderr {
				event.Event = &hostv1.ActionServiceRunResponse_Stderr{Stderr: append([]byte(nil), buffer[:read]...)}
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

// runAs resolves the account an action runs as.
//
// A local program's action runs as the program's own account, whatever the call
// asks for. The server may name an account, and otherwise gets the one the
// action belongs to.
func (h *actionService) runAs(caller *caller, action *action, want string) (*account, error) {
	if caller.role == roleUser {
		if want != "" && want != caller.account.Name {
			return nil, denied("agent: this socket runs actions as %s, not %s", caller.account.Name, want)
		}
		return caller.account, nil
	}
	if want != "" {
		acct, err := lookupAccount(want)
		if err != nil {
			return nil, toConnect(err)
		}
		return acct, nil
	}

	switch action.user {
	case hostv1.ActionUser_ACTION_USER_ROOT:
		acct, err := lookupAccount("root")
		if err != nil {
			return nil, toConnect(err)
		}
		return acct, nil
	case hostv1.ActionUser_ACTION_USER_OPERATOR:
		acct, err := lookupAccount(h.server.config.Operator)
		if err != nil {
			return nil, toConnect(err)
		}
		return acct, nil
	case hostv1.ActionUser_ACTION_USER_DESKTOP:
		acct, err := lookupAccount(h.server.config.DesktopUser)
		if err != nil {
			return nil, toConnect(err)
		}
		return acct, nil
	default:
		return caller.account, nil
	}
}

// environment is shared with action jobs run at the edge.
func (a *action) environment(encoded []byte) (map[string]string, error) {
	environment, err := hostaction.Environment(a.arguments, encoded)
	if err != nil {
		return nil, fmt.Errorf("%w: the arguments of %s: %w", ErrInvalid, a.name, err)
	}
	return environment, nil
}
