package hosts

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"strings"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/apply"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/hostaction"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/profile"
)

func coreActions(hostType profile.Type) []apply.Action {
	actions := []apply.Action{
		{Name: "status", Label: "Status", Group: "host", User: "operator", Run: "hostname; uptime"},
		{Name: "update", Label: "Update packages", Group: "host", User: "root", Run: "apt-get update && apt-get upgrade -y"},
		{Name: "reboot", Label: "Reboot", Group: "host", User: "root", Run: "systemctl --no-block reboot"},
	}
	if hostType == profile.TypeWorkstation {
		actions = append(actions, apply.Action{
			Name: "restart-desktop", Label: "Restart desktop", Group: "desktop", User: "desktop",
			Run: `uid=$(id -u); export XDG_RUNTIME_DIR="/run/user/$uid"; systemctl --user restart rookery-desktop.service rookery-box-desktop.service`,
		})
	}
	return actions
}

func actionMessage(action apply.Action) *hostv1.Action {
	actor := hostv1.ActionUser_ACTION_USER_OPERATOR
	switch action.User {
	case "root":
		actor = hostv1.ActionUser_ACTION_USER_ROOT
	case "desktop":
		actor = hostv1.ActionUser_ACTION_USER_DESKTOP
	}
	result := &hostv1.Action{Name: action.Name, Label: action.Label, Group: action.Group, User: actor, OnHost: action.OnHost, Terminal: action.Terminal}
	for _, argument := range action.Args {
		result.Arguments = append(result.Arguments, &hostv1.ActionArgument{
			Name: argument.Name, Label: argument.Label, Required: argument.Required, DefaultValue: argument.Default,
		})
	}
	return result
}

func (o *Operations) action(ctx context.Context, host Record, name string) (apply.Action, error) {
	for _, action := range coreActions(host.Type) {
		if action.Name == name {
			return action, nil
		}
	}
	resolved, err := o.resolve(ctx, host.Profile, host.Type)
	if err != nil {
		return apply.Action{}, err
	}
	for _, action := range resolved.actions {
		if action.Name == name {
			return action, nil
		}
	}
	return apply.Action{}, fmt.Errorf("%w: host %s has no action %q", access.ErrNotFound, host.Name, name)
}

type actionJobArgs struct {
	Action string         `cbor:"action"`
	Args   map[string]any `cbor:"args,omitempty"`
}

func encodeActionJob(name string, raw []byte) ([]byte, error) {
	args := actionJobArgs{Action: name}
	if len(raw) != 0 {
		if err := cborx.Unmarshal(raw, &args.Args); err != nil {
			return nil, fmt.Errorf("%w: action arguments: %w", access.ErrInvalid, err)
		}
	}
	return cborx.Marshal(args)
}

func (o *Operations) runAction(ctx context.Context, job jobs.Job, log *jobs.Log) ([]byte, error) {
	var args actionJobArgs
	if err := cborx.Unmarshal(job.Args, &args); err != nil {
		return nil, fmt.Errorf("%w: action arguments: %w", access.ErrInvalid, err)
	}
	if job.Host == "" || args.Action == "" {
		return nil, fmt.Errorf("%w: an action job needs a host resource and an action", access.ErrInvalid)
	}
	service := o.runtime.hosts
	host, err := service.deps.Hosts.Get(ctx, job.Host)
	if err != nil {
		return nil, err
	}
	if err := Check(host, CapabilityActions); err != nil {
		return nil, err
	}
	action, err := o.action(ctx, host, args.Action)
	if err != nil {
		return nil, err
	}
	if action.Terminal {
		return nil, fmt.Errorf("%w: action %q needs an interactive terminal", access.ErrInvalid, args.Action)
	}
	raw, err := cborx.Marshal(args.Args)
	if err != nil {
		return nil, err
	}
	env, err := hostaction.Environment(action.Args, raw)
	if err != nil {
		return nil, fmt.Errorf("%w: %w", access.ErrInvalid, err)
	}
	account, err := hostAccount(host, action.User)
	if err != nil {
		return nil, err
	}
	client, release, err := service.deps.Links.Use(ctx, host.Name)
	if err != nil {
		return nil, err
	}
	defer release()

	var stdout, stderr actionLog
	stdout.log, stderr.log = log, log
	var exit *hostv1.Exit
	err = runDaemonExec(ctx, client, &hostv1.ExecServiceRunRequest{
		User:    &hostv1.User{Name: account},
		Command: "/bin/sh", Args: []string{"-c", action.Run}, Env: env,
	}, func(event *hostv1.ExecServiceRunResponse) error { //nolint:contextcheck // the log owns the job context
		switch event := event.GetEvent().(type) {
		case *hostv1.ExecServiceRunResponse_Exit:
			exit = event.Exit
		case *hostv1.ExecServiceRunResponse_Stdout:
			return stdout.write(event.Stdout) //nolint:contextcheck // the log owns the job's context
		case *hostv1.ExecServiceRunResponse_Stderr:
			return stderr.write(event.Stderr) //nolint:contextcheck // the log owns the job's context
		}
		return nil
	})
	if err := errors.Join(err, stdout.flush(), stderr.flush()); err != nil { //nolint:contextcheck // the log owns the job's context
		return nil, err
	}
	if exit == nil {
		return nil, fmt.Errorf("%w: the action ended without an exit status", access.ErrUnavailable)
	}
	if code := exit.GetCode(); code != 0 {
		return nil, fmt.Errorf("hosts: action %s exited with status %d", action.Name, code)
	}
	return cborx.Marshal(map[string]int32{"exit_code": exit.GetCode()})
}

type actionLog struct {
	log     *jobs.Log
	pending []byte
}

func (l *actionLog) write(data []byte) error {
	const maxLine = 64 << 10
	for len(data) > 0 {
		part, remainder, complete := bytes.Cut(data, []byte{'\n'})
		if size := maxLine - len(l.pending); len(part) >= size {
			l.pending = append(l.pending, part[:size]...)
			data = data[size:]
			if err := l.flush(); err != nil {
				return err
			}
			if len(data) > 0 && data[0] == '\n' {
				data = data[1:]
			}
			continue
		}
		l.pending = append(l.pending, part...)
		if complete {
			if err := l.line(l.pending); err != nil {
				return err
			}
			l.pending = l.pending[:0]
		}
		data = remainder
	}
	return nil
}

func (l *actionLog) flush() error {
	if len(l.pending) == 0 {
		return nil
	}
	err := l.line(l.pending)
	l.pending = l.pending[:0]
	return err
}

func (l *actionLog) line(data []byte) error {
	value := strings.ToValidUTF8(strings.ReplaceAll(string(data), "\x00", ""), "�")
	return l.log.WriteLine(strings.TrimRight(value, "\r")) //nolint:contextcheck // the log owns the job's context
}
