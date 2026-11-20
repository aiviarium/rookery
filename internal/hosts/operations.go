package hosts

import (
	"context"
	"fmt"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/apply"
	"github.com/aiviarium/rookery/internal/config"
	"github.com/aiviarium/rookery/internal/hostaction"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/plugins"
	"github.com/aiviarium/rookery/internal/tailnet"
)

const (
	applyKind  = "host.apply"
	actionKind = "host.action"
)

// OperationOptions are the deployment dependencies needed for host changes.
type OperationOptions struct {
	Apply       *apply.Runner
	ReleaseDir  string
	Plugins     *plugins.Installer
	Secrets     *config.Secrets
	Connections *config.Connections

	// These are supplied by a configured tailnet.
	Tailnet        tailnet.Control
	TailnetTags    []string
	TailnetControl string
}

// Operations runs the host.apply and host.action jobs.
type Operations struct {
	runtime *Runtime
	options OperationOptions
}

// RegisterOperations wires the operations and registers their core job handlers.
func (s *Service) RegisterOperations(runtime *Runtime, registry *jobs.Registry, options OperationOptions) error {
	operations := &Operations{runtime: runtime, options: options}
	if err := registry.Handle(applyKind, jobs.HandlerFunc(operations.apply)); err != nil {
		return err
	}
	if err := registry.Handle(actionKind, jobs.HandlerFunc(operations.runAction)); err != nil {
		return err
	}
	s.operations = operations
	return nil
}

func (s *Service) enqueueOperation(ctx context.Context, name, kind string, args []byte) (jobs.Job, error) {
	if s.deps.Jobs == nil {
		return jobs.Job{}, fmt.Errorf("%w: host operations need the job runner", access.ErrUnavailable)
	}
	return s.deps.Jobs.Enqueue(ctx, jobs.EnqueueRequest{Kind: kind, Args: args, Resource: access.HostURI(name)})
}

// Apply queues a host apply and returns its job id.
func (s *Service) Apply(ctx context.Context, request *connect.Request[rookeryv1.HostServiceApplyRequest]) (*connect.Response[rookeryv1.HostServiceApplyResponse], error) {
	record, err := s.authorizeHost(ctx, request.Msg.GetName(), access.Manage, CapabilityApply)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	job, err := s.enqueueOperation(ctx, record.Name, applyKind, nil)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.HostServiceApplyResponse{Job: job.ID}), nil
}

// ListActions returns the core and feature actions applicable to the host.
func (s *Service) ListActions(ctx context.Context, request *connect.Request[rookeryv1.HostServiceListActionsRequest]) (*connect.Response[rookeryv1.HostServiceListActionsResponse], error) {
	host, err := s.authorizeHost(ctx, request.Msg.GetName(), access.View, CapabilityActions)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	if s.operations == nil {
		return nil, access.ToConnect(access.ErrUnavailable)
	}
	resolved, err := s.operations.resolve(ctx, host.Profile, host.Type)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	response := &rookeryv1.HostServiceListActionsResponse{Actions: make([]*hostv1.Action, 0, len(resolved.actions))}
	for _, action := range resolved.actions {
		response.Actions = append(response.Actions, actionMessage(action))
	}
	return connect.NewResponse(response), nil
}

// RunAction enqueues a job or opens the action on a PTY if it is interactive.
func (s *Service) RunAction(ctx context.Context, request *connect.Request[rookeryv1.HostServiceRunActionRequest]) (*connect.Response[rookeryv1.HostServiceRunActionResponse], error) {
	record, err := s.authorizeHost(ctx, request.Msg.GetName(), access.Manage, CapabilityActions)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	if s.operations == nil {
		return nil, access.ToConnect(access.ErrUnavailable)
	}
	action, err := s.operations.action(ctx, record, request.Msg.GetAction())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	environment, err := hostaction.Environment(action.Args, request.Msg.GetArgsCbor())
	if err != nil {
		return nil, access.ToConnect(fmt.Errorf("%w: %w", access.ErrInvalid, err))
	}
	if action.Terminal {
		account, err := hostAccount(record, action.User)
		if err != nil {
			return nil, access.ToConnect(err)
		}
		term := &TerminalService{s.operations.runtime}
		opened, err := term.open(ctx, record.Name, &hostv1.SessionServiceCreateRequest{
			User: &hostv1.User{Name: account}, Command: "/bin/sh", Args: []string{"-c", action.Run}, Env: environment,
			Sharing: &hostv1.SessionSharing{Owner: access.MustPrincipal(ctx).String(), Elevated: true, Private: true},
		})
		if err != nil {
			return nil, access.ToConnect(err)
		}
		return connect.NewResponse(&rookeryv1.HostServiceRunActionResponse{
			Execution: &rookeryv1.HostServiceRunActionResponse_Terminal{Terminal: opened},
		}), nil
	}
	raw, err := encodeActionJob(request.Msg.GetAction(), request.Msg.GetArgsCbor())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	job, err := s.enqueueOperation(ctx, record.Name, actionKind, raw)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.HostServiceRunActionResponse{
		Execution: &rookeryv1.HostServiceRunActionResponse_Job{Job: job.ID},
	}), nil
}
