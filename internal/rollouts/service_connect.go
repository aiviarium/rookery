package rollouts

import (
	"context"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
)

// RolloutService is RolloutService, the service: Plan, Start, List, Get, Watch,
// Pause, Resume and Cancel.
type RolloutService struct {
	rollouts *Service
}

// NewRolloutService returns RolloutService for a roll-out service.
func NewRolloutService(service *Service) *RolloutService {
	return &RolloutService{rollouts: service}
}

// Plan resolves a selection into the hosts a roll-out would work.
func (s *RolloutService) Plan(ctx context.Context, request *connect.Request[rookeryv1.RolloutServicePlanRequest]) (*connect.Response[rookeryv1.RolloutServicePlanResponse], error) {
	if _, ok := access.PrincipalFrom(ctx); !ok {
		return nil, access.ToConnect(access.ErrUnauthenticated)
	}
	plan, err := s.rollouts.Plan(ctx, request.Msg.GetKind(), selectionOf(request.Msg.GetSelection()))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := &rookeryv1.RolloutServicePlanResponse{
		Hosts:   make([]*rookeryv1.RolloutHost, 0, len(plan.Hosts)),
		Skipped: make([]*rookeryv1.RolloutHost, 0, len(plan.Skipped)),
	}
	for _, host := range plan.Hosts {
		out.Hosts = append(out.Hosts, host.Message())
	}
	for _, host := range plan.Skipped {
		out.Skipped = append(out.Skipped, host.Message())
	}
	return connect.NewResponse(out), nil
}

// Start makes a roll-out.
func (s *RolloutService) Start(ctx context.Context, request *connect.Request[rookeryv1.RolloutServiceStartRequest]) (*connect.Response[rookeryv1.RolloutServiceStartResponse], error) {
	rollout, err := s.rollouts.Start(ctx, StartRequest{
		Kind:      request.Msg.GetKind(),
		Args:      request.Msg.GetArgsCbor(),
		Resource:  request.Msg.GetResource(),
		Selection: selectionOf(request.Msg.GetSelection()),
		Strategy:  strategyOf(request.Msg.GetStrategy()),
	})
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.RolloutServiceStartResponse{Rollout: rollout.Message()}), nil
}

// List returns a page of roll-outs.
func (s *RolloutService) List(ctx context.Context, request *connect.Request[rookeryv1.RolloutServiceListRequest]) (*connect.Response[rookeryv1.RolloutServiceListResponse], error) {
	found, err := s.rollouts.List(ctx, request.Msg.GetState(), request.Msg.GetCursor(), int(request.Msg.GetLimit()))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := make([]*rookeryv1.Rollout, 0, len(found))
	for _, rollout := range found {
		out = append(out, rollout.Message())
	}
	next := ""
	limit := int(request.Msg.GetLimit())
	if limit <= 0 {
		limit = 100
	}
	if len(found) == limit {
		next = found[len(found)-1].ID
	}
	return connect.NewResponse(&rookeryv1.RolloutServiceListResponse{Rollouts: out, NextCursor: next}), nil
}

// Get returns one roll-out.
func (s *RolloutService) Get(ctx context.Context, request *connect.Request[rookeryv1.RolloutServiceGetRequest]) (*connect.Response[rookeryv1.RolloutServiceGetResponse], error) {
	rollout, err := s.rollouts.Get(ctx, request.Msg.GetRollout())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.RolloutServiceGetResponse{Rollout: rollout.Message()}), nil
}

// Watch streams a roll-out until it has ended.
func (s *RolloutService) Watch(ctx context.Context, request *connect.Request[rookeryv1.RolloutServiceWatchRequest], stream *connect.ServerStream[rookeryv1.RolloutServiceWatchResponse]) error {
	return access.ToConnect(s.rollouts.Watch(ctx, request.Msg.GetRollout(), func(rollout Rollout) error {
		return stream.Send(&rookeryv1.RolloutServiceWatchResponse{Rollout: rollout.Message()})
	}))
}

// Pause stops a roll-out.
func (s *RolloutService) Pause(ctx context.Context, request *connect.Request[rookeryv1.RolloutServicePauseRequest]) (*connect.Response[rookeryv1.RolloutServicePauseResponse], error) {
	why := request.Msg.GetWhy()
	if why == "" {
		why = "paused by a caller"
	}
	rollout, err := s.rollouts.Pause(ctx, request.Msg.GetRollout(), why)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.RolloutServicePauseResponse{Rollout: rollout.Message()}), nil
}

// Resume lets a paused roll-out carry on.
func (s *RolloutService) Resume(ctx context.Context, request *connect.Request[rookeryv1.RolloutServiceResumeRequest]) (*connect.Response[rookeryv1.RolloutServiceResumeResponse], error) {
	rollout, err := s.rollouts.Resume(ctx, request.Msg.GetRollout())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.RolloutServiceResumeResponse{Rollout: rollout.Message()}), nil
}

// Cancel stops a roll-out, cancelling the jobs that are running.
func (s *RolloutService) Cancel(ctx context.Context, request *connect.Request[rookeryv1.RolloutServiceCancelRequest]) (*connect.Response[rookeryv1.RolloutServiceCancelResponse], error) {
	rollout, err := s.rollouts.Cancel(ctx, request.Msg.GetRollout())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.RolloutServiceCancelResponse{Rollout: rollout.Message()}), nil
}
