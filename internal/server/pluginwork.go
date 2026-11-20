package server

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"time"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/types/known/timestamppb"

	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/jobs"
)

// Work delivers one outstanding claim per stream. Further claims wait until
// the worker has reported an outcome or lost the previous one.
func (p *PluginServer) Work(ctx context.Context, request *connect.Request[pluginv1.ServerServiceWorkRequest], stream *connect.ServerStream[pluginv1.ServerServiceWorkResponse]) error {
	plugin, err := requirePlugin(ctx)
	if err != nil {
		return err
	}
	kinds, err := p.workKinds(ctx, plugin, request.Msg.GetKinds())
	if err != nil {
		return access.ToConnect(err)
	}
	if err := stream.Send(nil); err != nil {
		return err
	}
	for {
		claim, err := p.worker.Claim(ctx, kinds, time.Second)
		if err != nil {
			return access.ToConnect(err)
		}
		if claim == nil {
			if _, err := p.installer.Manifest(ctx, plugin); err != nil {
				return access.ToConnect(err)
			}
			continue
		}
		message := &pluginv1.ServerServiceWorkResponse{
			Job: jobs.Message(claim.Job), ClaimToken: claim.Token,
			Deadline: timestamppb.New(claim.Deadline),
		}
		if user, ok := strings.CutPrefix(claim.Job.CreatedBy, "user:"); ok {
			space, ok := strings.CutPrefix(claim.Resource, "space://")
			if !ok {
				space = ""
			}
			message.CallerToken = p.callers.Issue(plugin, user, space, claim.Deadline)
		}
		if err := stream.Send(message); err != nil {
			return err
		}
		if err := p.waitForClaim(ctx, plugin, claim); err != nil {
			return access.ToConnect(err)
		}
	}
}

func (p *PluginServer) workKinds(ctx context.Context, plugin string, requested []string) ([]string, error) {
	manifest, err := p.installer.Manifest(ctx, plugin)
	if err != nil {
		return nil, err
	}
	known := make(map[string]bool, len(manifest.Jobs))
	for _, job := range manifest.Jobs {
		known[job.Kind] = true
	}
	if len(requested) == 0 {
		for _, job := range manifest.Jobs {
			requested = append(requested, job.Kind)
		}
	}
	if len(requested) == 0 {
		return nil, fmt.Errorf("%w: the plugin declares no job kinds", access.ErrInvalid)
	}
	kinds := make([]string, 0, len(requested))
	for _, name := range requested {
		if !known[name] {
			return nil, fmt.Errorf("%w: the plugin did not declare job kind %q", access.ErrInvalid, name)
		}
		kinds = append(kinds, plugin+"/"+name)
	}
	return kinds, nil
}

func (p *PluginServer) waitForClaim(ctx context.Context, plugin string, initial *jobs.Claim) error {
	ticker := time.NewTicker(250 * time.Millisecond)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
			claim, err := p.worker.OwnedClaim(ctx, plugin, initial.ID, initial.Token)
			switch {
			case errors.Is(err, access.ErrConflict):
				return nil
			case err != nil:
				return err
			case claim.Job.CancelAsked:
				return ignoreLostClaim(p.worker.Cancel(ctx, claim))
			case time.Now().After(claim.Deadline):
				return ignoreLostClaim(p.worker.Fail(ctx, claim, errors.New("worker deadline exceeded")))
			}
		}
	}
}

func ignoreLostClaim(err error) error {
	if errors.Is(err, access.ErrConflict) {
		return nil
	}
	return err
}

// ReportWork updates a claim held by the authenticated plugin.
func (p *PluginServer) ReportWork(ctx context.Context, request *connect.Request[pluginv1.ServerServiceReportWorkRequest]) (*connect.Response[pluginv1.ServerServiceReportWorkResponse], error) {
	plugin, err := requirePlugin(ctx)
	if err != nil {
		return nil, err
	}
	message := request.Msg
	if message.GetJobId() == "" || message.GetClaimToken() == "" {
		return nil, access.ToConnect(access.ErrInvalid)
	}
	claim, err := p.worker.OwnedClaim(ctx, plugin, message.GetJobId(), message.GetClaimToken())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	if claim.Job.CancelAsked || time.Now().After(claim.Deadline) {
		return nil, access.ToConnect(fmt.Errorf("%w: the job was cancelled or reached its deadline", access.ErrConflict))
	}
	switch report := message.GetReport().(type) {
	case *pluginv1.ServerServiceReportWorkRequest_Heartbeat:
		err = p.worker.Heartbeat(ctx, claim)
	case *pluginv1.ServerServiceReportWorkRequest_Line:
		var line string
		line, err = p.maskJobText(ctx, plugin, claim.Token, report.Line)
		if err == nil {
			err = p.worker.Line(ctx, claim, line)
		}
	case *pluginv1.ServerServiceReportWorkRequest_ProgressCbor:
		err = validatePluginCBOR(report.ProgressCbor)
		if err == nil {
			err = p.worker.Progress(ctx, claim, report.ProgressCbor)
		}
	case *pluginv1.ServerServiceReportWorkRequest_ResultCbor:
		err = validatePluginCBOR(report.ResultCbor)
		if err == nil {
			err = p.worker.Complete(ctx, claim, report.ResultCbor)
		}
	case *pluginv1.ServerServiceReportWorkRequest_Error:
		var reason string
		reason, err = p.maskJobText(ctx, plugin, claim.Token, report.Error)
		if err == nil {
			err = p.worker.Fail(ctx, claim, errors.New(reason))
		}
	default:
		err = access.ErrInvalid
	}
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&pluginv1.ServerServiceReportWorkResponse{}), nil
}

func (p *PluginServer) maskJobText(ctx context.Context, plugin, claim, text string) (string, error) {
	manifest, err := p.installer.Manifest(ctx, plugin)
	if err != nil {
		return "", err
	}
	masker := jobs.NewMasker(claim)

	for _, name := range manifest.Secrets {
		secret, err := p.secrets.Value(ctx, name, "plugin/"+plugin)
		if errors.Is(err, access.ErrNotFound) {
			continue
		}
		if err != nil {
			return "", err
		}
		masker.Add(string(secret))
	}
	return masker.Mask(text), nil
}
