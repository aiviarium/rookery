package jobs

import (
	"context"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/store"
)

// JobService is JobService, the service: Enqueue, List, Get, Follow, Cancel and
// Retry.
type JobService struct {
	jobs *Service
}

// NewJobService returns JobService for a database.
func NewJobService(service *Service) *JobService { return &JobService{jobs: service} }

// Enqueue makes a job.
func (s *JobService) Enqueue(ctx context.Context, request *connect.Request[rookeryv1.JobServiceEnqueueRequest]) (*connect.Response[rookeryv1.JobServiceEnqueueResponse], error) {
	job, err := s.jobs.Enqueue(ctx, EnqueueRequest{
		Kind:     request.Msg.GetKind(),
		Args:     request.Msg.GetArgsCbor(),
		Resource: request.Msg.GetResource(),
		Parent:   request.Msg.GetParent(),
	})
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.JobServiceEnqueueResponse{Job: Message(job)}), nil
}

// List returns a page of jobs.
func (s *JobService) List(ctx context.Context, request *connect.Request[rookeryv1.JobServiceListRequest]) (*connect.Response[rookeryv1.JobServiceListResponse], error) {
	found, err := s.jobs.List(ctx, Filter{
		Kind:     request.Msg.GetKind(),
		State:    request.Msg.GetState(),
		Resource: request.Msg.GetResource(),
		Cursor:   request.Msg.GetCursor(),
		Limit:    int(request.Msg.GetLimit()),
	})
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := make([]*rookeryv1.Job, 0, len(found))
	for _, job := range found {
		out = append(out, Message(job))
	}
	next := ""
	limit := int(request.Msg.GetLimit())
	if limit <= 0 {
		limit = 100
	}
	if len(found) == limit {
		next = found[len(found)-1].ID
	}
	return connect.NewResponse(&rookeryv1.JobServiceListResponse{Jobs: out, NextCursor: next}), nil
}

// Get returns one job.
func (s *JobService) Get(ctx context.Context, request *connect.Request[rookeryv1.JobServiceGetRequest]) (*connect.Response[rookeryv1.JobServiceGetResponse], error) {
	job, err := s.jobs.Get(ctx, request.Msg.GetJob())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.JobServiceGetResponse{Job: Message(job)}), nil
}

// Follow streams a job's lines from an offset, and then its end state.
func (s *JobService) Follow(ctx context.Context, request *connect.Request[rookeryv1.JobServiceFollowRequest], stream *connect.ServerStream[rookeryv1.JobServiceFollowResponse]) error {
	// A job that is not there is an error before the stream starts, so a caller
	// sees NotFound rather than an empty stream.
	if _, err := s.jobs.Get(ctx, request.Msg.GetJob()); err != nil {
		return access.ToConnect(err)
	}

	return s.jobs.Follow(ctx, request.Msg.GetJob(), request.Msg.GetFrom(), func(line store.RookeryJobLines, done bool, job Job) error {
		if done {
			return stream.Send(&rookeryv1.JobServiceFollowResponse{
				Event: &rookeryv1.JobServiceFollowResponse_Job{Job: Message(job)},
			})
		}
		return stream.Send(&rookeryv1.JobServiceFollowResponse{
			Event: &rookeryv1.JobServiceFollowResponse_Line{Line: &rookeryv1.JobLine{
				Seq:  line.ID,
				Line: line.Line,
				At:   timestamppb.New(line.At),
			}},
		})
	})
}

// Cancel asks a job to stop.
func (s *JobService) Cancel(ctx context.Context, request *connect.Request[rookeryv1.JobServiceCancelRequest]) (*connect.Response[rookeryv1.JobServiceCancelResponse], error) {
	job, err := s.jobs.Cancel(ctx, request.Msg.GetJob())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.JobServiceCancelResponse{Job: Message(job)}), nil
}

// Retry puts a job back in the queue.
func (s *JobService) Retry(ctx context.Context, request *connect.Request[rookeryv1.JobServiceRetryRequest]) (*connect.Response[rookeryv1.JobServiceRetryResponse], error) {
	job, err := s.jobs.Retry(ctx, request.Msg.GetJob())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.JobServiceRetryResponse{Job: Message(job)}), nil
}
