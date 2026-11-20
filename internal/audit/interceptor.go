package audit

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"time"

	"connectrpc.com/connect"

	"github.com/aiviarium/rookery/internal/access"
)

// recordTimeout bounds writing an entry after the call has ended, when the call's
// own context may already be cancelled.
const recordTimeout = 5 * time.Second

// Interceptor records a call that changes state, and every refused call.
//
// A procedure changes state when it needs manage on a resource, or the admin or
// local level: those are the ones that write. A refusal is recorded at any level,
// because a refused attempt is what an audit trail is for.
type Interceptor struct {
	recorder *Recorder
	logger   *slog.Logger
}

// NewInterceptor returns the audit interceptor. It logs a failed write, since an
// entry missing from the trail is worth knowing about, and never fails a call for
// it.
func NewInterceptor(recorder *Recorder, logger *slog.Logger) *Interceptor {
	return &Interceptor{recorder: recorder, logger: logger}
}

// WrapUnary records a unary call.
func (i *Interceptor) WrapUnary(next connect.UnaryFunc) connect.UnaryFunc {
	return func(ctx context.Context, request connect.AnyRequest) (connect.AnyResponse, error) {
		response, err := next(ctx, request)
		i.record(ctx, request.Spec(), request.Any(), err)
		return response, err
	}
}

// WrapStreamingHandler records a streaming call when it ends.
func (i *Interceptor) WrapStreamingHandler(next connect.StreamingHandlerFunc) connect.StreamingHandlerFunc {
	return func(ctx context.Context, conn connect.StreamingHandlerConn) error {
		err := next(ctx, conn)
		i.record(ctx, conn.Spec(), nil, err)
		return err
	}
}

// WrapStreamingClient passes a call through: this side is a client.
func (i *Interceptor) WrapStreamingClient(next connect.StreamingClientFunc) connect.StreamingClientFunc {
	return next
}

// record writes the entry, when the call is one the trail keeps. request is the
// call's message, or nil for a stream, whose message this interceptor does not
// read.
func (i *Interceptor) record(ctx context.Context, spec connect.Spec, request any, callErr error) {
	method, err := access.MethodOf(spec)
	if err != nil {
		i.logf("audit: reading the procedure of %s: %v", spec.Procedure, err)
		return
	}
	level, field, err := access.MethodOption(method)
	if err != nil {
		i.logf("audit: reading the access of %s: %v", spec.Procedure, err)
		return
	}

	resource := access.ResourceFrom(ctx)
	if request != nil {
		if resolved, resolveErr := access.ResourceOf(method, request, field); resolveErr == nil {
			resource = resolved
		}
	}

	outcome := OK
	if callErr != nil {
		outcome = Error
		var connectErr *connect.Error
		if errors.As(callErr, &connectErr) {
			switch connectErr.Code() {
			case connect.CodePermissionDenied, connect.CodeUnauthenticated:
				outcome = Denied
			case connect.CodeUnavailable:
				if refusal := access.RefusalOf(callErr); refusal != nil {
					outcome = Denied
				}
			}
		}
	}

	changes := access.Mutates(method) || level == access.Manage || level == access.Admin || level == access.Local
	if !changes && outcome == OK {
		return
	}

	principal, _ := access.PrincipalFrom(ctx)
	entry := Entry{
		Principal: principal.String(),
		Procedure: spec.Procedure,
		Resource:  resource,
		Outcome:   outcome,
	}
	if callErr != nil {
		entry.Detail = callErr.Error()
	}

	recordCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), recordTimeout)
	defer cancel()
	if err := i.recorder.Record(recordCtx, entry); err != nil {
		i.logf("audit: %v", err)
	}
}

// logf writes a line without failing a call over it.
func (i *Interceptor) logf(format string, args ...any) {
	if i.logger != nil {
		i.logger.Warn("audit", slog.String("problem", fmt.Sprintf(format, args...)))
	}
}
