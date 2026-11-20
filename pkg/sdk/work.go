package sdk

import (
	"context"
	"errors"
	"time"

	"connectrpc.com/connect"

	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
)

// Work is one job attempt with its claim and delegated caller.
type Work struct {
	Job    *rookeryv1.Job
	Caller *Client
	client *Client
	token  string
}

// Line adds a line to the job log.
func (w Work) Line(ctx context.Context, line string) error {
	req := w.report()
	req.Report = &pluginv1.ServerServiceReportWorkRequest_Line{Line: line}
	return w.send(ctx, req)
}

// Progress publishes the job's most recent structured progress.
func (w Work) Progress(ctx context.Context, progress any) error {
	value, err := Marshal(progress)
	if err != nil {
		return err
	}
	req := w.report()
	req.Report = &pluginv1.ServerServiceReportWorkRequest_ProgressCbor{ProgressCbor: value}
	return w.send(ctx, req)
}

func (w Work) report() *pluginv1.ServerServiceReportWorkRequest {
	return &pluginv1.ServerServiceReportWorkRequest{JobId: w.Job.GetId(), ClaimToken: w.token}
}

func (w Work) send(ctx context.Context, report *pluginv1.ServerServiceReportWorkRequest) error {
	_, err := w.client.server.ReportWork(ctx, connect.NewRequest(report))
	return err
}

// RunWorker processes one job at a time, reporting its result and keeping its
// claim alive while handle runs. Run it with the plugin's lifetime.
func (c *Client) RunWorker(ctx context.Context, kinds []string, handle func(context.Context, Work) (any, error)) error {
	stream, err := c.server.Work(ctx, connect.NewRequest(&pluginv1.ServerServiceWorkRequest{Kinds: kinds}))
	if err != nil {
		return err
	}
	defer func() { _ = stream.Close() }()
	for stream.Receive() {
		claim := stream.Msg()
		if err := c.runClaim(ctx, claim, handle); err != nil && connect.CodeOf(err) != connect.CodeAborted {
			return err
		}
	}
	return stream.Err()
}

func (c *Client) runClaim(ctx context.Context, claim *pluginv1.ServerServiceWorkResponse, handle func(context.Context, Work) (any, error)) error {
	job := Work{Job: claim.GetJob(), Caller: c.AsCaller(claim.GetCallerToken()), client: c, token: claim.GetClaimToken()}
	jobCtx, cancel := context.WithDeadline(ctx, claim.GetDeadline().AsTime())
	defer cancel()
	heartbeatDone := make(chan error, 1)
	go func() {
		heartbeatDone <- job.heartbeat(jobCtx)
		cancel()
	}()
	result, failure := handle(jobCtx, job)
	cancel()
	if err := <-heartbeatDone; err != nil && !errors.Is(err, context.Canceled) && !errors.Is(err, context.DeadlineExceeded) {
		return err
	}
	if ctx.Err() != nil {
		return ctx.Err()
	}
	req := job.report()
	if failure == nil {
		var resultCBOR []byte
		resultCBOR, failure = Marshal(result)
		if failure == nil {
			req.Report = &pluginv1.ServerServiceReportWorkRequest_ResultCbor{ResultCbor: resultCBOR}
		}
	}
	if failure != nil {
		req.Report = &pluginv1.ServerServiceReportWorkRequest_Error{Error: failure.Error()}
	}
	return job.send(ctx, req)
}

func (w Work) heartbeat(ctx context.Context) error {
	ticker := time.NewTicker(5 * time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return nil
		case <-ticker.C:
			req := w.report()
			req.Report = &pluginv1.ServerServiceReportWorkRequest_Heartbeat{Heartbeat: true}
			if err := w.send(ctx, req); err != nil {
				return err
			}
		}
	}
}
