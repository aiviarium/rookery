package hosts

import (
	"bytes"
	"context"
	"fmt"
	"sync"
	"time"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/proto"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/link"
	"github.com/aiviarium/rookery/internal/profile"
)

// ExecService streams a command's output and exit status.
type ExecService struct{ *Runtime }

// Run streams standard output, standard error and the process exit status.
func (s *ExecService) Run(ctx context.Context, request *connect.Request[rookeryv1.ExecServiceRunRequest], stream *connect.ServerStream[rookeryv1.ExecServiceRunResponse]) error {
	name, command := request.Msg.GetHost(), request.Msg.GetExec()
	record, err := s.hosts.authorizeHost(ctx, name, access.Use, CapabilityExec)
	if err != nil {
		return access.ToConnect(err)
	}
	if command.GetCommand() == "" {
		return access.ToConnect(fmt.Errorf("%w: exec needs a command", access.ErrInvalid))
	}
	send := func(event *hostv1.ExecServiceRunResponse) error {
		return stream.Send(&rookeryv1.ExecServiceRunResponse{Event: event})
	}
	return access.ToConnect(s.run(ctx, record, command, send))
}

func (s *ExecService) run(ctx context.Context, record Record, command *hostv1.ExecServiceRunRequest, send func(*hostv1.ExecServiceRunResponse) error) error {
	if command.GetDesktop() {
		if err := Check(record, CapabilityDesktop); err != nil {
			return err
		}
	}
	selector := command.GetUser().GetName()
	if record.Type == profile.TypeSSH {
		return s.runSSH(ctx, record, command, selector, send)
	}

	user, _, err := s.account(ctx, record, selector)
	if err != nil {
		return err
	}
	remote := proto.CloneOf(command)
	remote.User = user
	client, release, err := s.hosts.deps.Links.Use(ctx, record.Name)
	if err != nil {
		return err
	}
	defer release()
	return runDaemonExec(ctx, client, remote, send)
}

func runDaemonExec(ctx context.Context, client *link.Client, command *hostv1.ExecServiceRunRequest, send func(*hostv1.ExecServiceRunResponse) error) error {
	stream, err := client.Exec.Run(ctx, connect.NewRequest(command))
	if err != nil {
		return err
	}
	defer func() { _ = stream.Close() }()
	for stream.Receive() {
		if err := send(stream.Msg()); err != nil {
			return err
		}
	}
	return stream.Err()
}

func (s *ExecService) runSSH(ctx context.Context, record Record, command *hostv1.ExecServiceRunRequest, selector string, send func(*hostv1.ExecServiceRunResponse) error) error {
	principal, _ := access.PrincipalFrom(ctx)
	if err := s.hosts.deps.Checker.Check(ctx, principal, record.URI(), access.Manage); err != nil {
		return err
	}
	if selector != "" && selector != "operator" && selector != "root" {
		return fmt.Errorf("%w: SSH-only hosts run as the SSH user or root", access.ErrInvalid)
	}
	target := targetOf(record)
	if timeout := command.GetTimeoutSeconds(); timeout > 0 {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, time.Duration(timeout)*time.Second)
		defer cancel()
	}
	var mu sync.Mutex
	out := execWriter{mu: &mu, send: send}
	stderr := execWriter{mu: &mu, send: send, stderr: true}
	exit, err := s.hosts.deps.Links.RunSSH(ctx, target, link.Command{
		Program: command.GetCommand(), Args: command.GetArgs(), Dir: command.GetDir(),
		Env: command.GetEnv(), Root: selector == "root", Stdin: bytes.NewReader(command.GetStdin()),
		Stdout: out, Stderr: stderr,
	})
	if err != nil {
		return err
	}
	return send(&hostv1.ExecServiceRunResponse{Event: &hostv1.ExecServiceRunResponse_Exit{
		Exit: &hostv1.Exit{Code: int32(exit)}, //nolint:gosec // SSH exit status is a process exit code
	}})
}

type execWriter struct {
	mu     *sync.Mutex
	send   func(*hostv1.ExecServiceRunResponse) error
	stderr bool
}

func (w execWriter) Write(data []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	event := &hostv1.ExecServiceRunResponse{Event: &hostv1.ExecServiceRunResponse_Stdout{Stdout: data}}
	if w.stderr {
		event.Event = &hostv1.ExecServiceRunResponse_Stderr{Stderr: data}
	}
	if err := w.send(event); err != nil {
		return 0, err
	}
	return len(data), nil
}
