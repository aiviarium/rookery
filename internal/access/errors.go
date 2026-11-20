package access

import (
	"context"
	"errors"
	"fmt"

	"connectrpc.com/connect"
	"github.com/jackc/pgx/v5"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
)

// Domain errors. Every package wraps one of these, and ToConnect is the only
// place that turns them into a connect code.
var (
	// ErrInvalid is a malformed or impossible request: InvalidArgument.
	ErrInvalid = errors.New("access: invalid argument")
	// ErrUnauthenticated is a caller who did not sign in, or whose session or
	// token is no longer good: Unauthenticated.
	ErrUnauthenticated = errors.New("access: not signed in")
	// ErrDenied is a caller who may not do this: PermissionDenied.
	ErrDenied = errors.New("access: permission denied")
	// ErrNotFound is a row or a resource that is not there: NotFound.
	ErrNotFound = errors.New("access: not found")
	// ErrConflict is a revision or a lease that moved: Aborted.
	ErrConflict = errors.New("access: conflict")
	// ErrUnavailable is a host that is offline, a capability that is false, or
	// a standby: Unavailable.
	ErrUnavailable = errors.New("access: unavailable")
)

// Denied is a refusal that says what was missing. It carries a
// rookery.v1.Refusal to the caller.
type Denied struct {
	// Resource is the resource URI the call was about, when it named one.
	Resource string
	// Need is the level the caller lacked.
	Need Level
	// Capability is the host capability that was false, if any.
	Capability string
	// Reason says what would change the answer, in words fit for the caller.
	Reason string
}

// Error is the refusal in words, for a log line.
func (d *Denied) Error() string {
	what := "this"
	if d.Resource != "" {
		what = d.Resource
	}
	reason := d.Reason
	if reason == "" {
		reason = "ask an admin for access"
	}
	if d.Capability != "" {
		return fmt.Sprintf("access: %s needs the %s capability: %s", what, d.Capability, reason)
	}
	return fmt.Sprintf("access: %s needs %s: %s", what, LevelName(d.Need), reason)
}

// Unwrap makes the refusal an ErrDenied.
func (d *Denied) Unwrap() error { return ErrDenied }

// ToConnect is the only place a domain error becomes a connect code. A refusal
// carries rookery.v1.Refusal as its error detail, and an error that already is
// a connect error keeps its code.
func ToConnect(err error) error {
	if err == nil {
		return nil
	}

	var connectErr *connect.Error
	if errors.As(err, &connectErr) {
		return connectErr
	}

	code := connect.CodeInternal
	var denied *Denied
	switch {
	case errors.As(err, &denied):
		code = connect.CodePermissionDenied
	case errors.Is(err, ErrInvalid):
		code = connect.CodeInvalidArgument
	case errors.Is(err, ErrUnauthenticated):
		code = connect.CodeUnauthenticated
	case errors.Is(err, ErrDenied):
		code = connect.CodePermissionDenied
	case errors.Is(err, ErrNotFound), errors.Is(err, pgx.ErrNoRows):
		code = connect.CodeNotFound
	case errors.Is(err, ErrConflict):
		code = connect.CodeAborted
	case errors.Is(err, ErrUnavailable):
		code = connect.CodeUnavailable
	case errors.Is(err, context.DeadlineExceeded):
		code = connect.CodeDeadlineExceeded
	case errors.Is(err, context.Canceled):
		code = connect.CodeCanceled
	}

	out := connect.NewError(code, err)
	if denied == nil {
		return out
	}
	detail, detailErr := connect.NewErrorDetail(&rookeryv1.Refusal{
		Resource:   denied.Resource,
		Level:      denied.Need,
		Capability: denied.Capability,
		Reason:     denied.Reason,
	})
	if detailErr == nil {
		out.AddDetail(detail)
	}
	return out
}

// RefusalOf reads the Refusal a refused call carries, for a test or a client
// that shows why a call was refused.
func RefusalOf(err error) *rookeryv1.Refusal {
	var connectErr *connect.Error
	if !errors.As(err, &connectErr) {
		return nil
	}
	for _, detail := range connectErr.Details() {
		message, valueErr := detail.Value()
		if valueErr != nil {
			continue
		}
		if refusal, ok := message.(*rookeryv1.Refusal); ok {
			return refusal
		}
	}
	return nil
}

// malformed is the error for an access option the interceptor cannot read: a
// method that declares none, or one that names a field the request does not
// have. It is a bug in the service, not in the call.
func malformed(format string, args ...any) error {
	return connect.NewError(connect.CodeInternal, fmt.Errorf("access: "+format, args...))
}
