package agent

import (
	"context"
	"errors"
	"fmt"
	"os"

	"connectrpc.com/connect"
)

// The package's domain errors. Handlers wrap them, and toConnect maps them to
// connect codes in one place.
var (
	// ErrInvalid is a call whose arguments do not make sense.
	ErrInvalid = errors.New("agent: invalid request")

	// ErrNotPermitted is a call the caller's account may not make.
	ErrNotPermitted = errors.New("agent: not permitted")

	// ErrUnavailable is something the host does not have, such as a host service
	// that is not registered.
	ErrUnavailable = errors.New("agent: unavailable")

	// ErrRevisionConflict is a write whose revision is not the file's.
	ErrRevisionConflict = errors.New("agent: the file's revision is not the one the write expects")

	// ErrExists is a write that was told not to overwrite.
	ErrExists = errors.New("agent: the path exists")

	// ErrNoDesktop is a desktop call on a host with no desktop provider.
	ErrNoDesktop = errors.New("agent: no desktop provider is registered on this host")
)

// invalid, denied, unauthenticated, unavailable and failed build the connect
// errors handlers return without going through a sentinel.
func invalid(format string, args ...any) error {
	return connect.NewError(connect.CodeInvalidArgument, fmt.Errorf(format, args...))
}

func denied(format string, args ...any) error {
	return connect.NewError(connect.CodePermissionDenied, fmt.Errorf(format, args...))
}

func unauthenticated(err error) error {
	return connect.NewError(connect.CodeUnauthenticated, err)
}

// toConnect maps the package's errors to connect codes. It is the one place in
// rookeryd that does, as access.ToConnect is on the server.
func toConnect(err error) error {
	if err == nil {
		return nil
	}
	var already *connect.Error
	if errors.As(err, &already) {
		return err
	}

	switch {
	case errors.Is(err, context.Canceled):
		return connect.NewError(connect.CodeCanceled, err)
	case errors.Is(err, context.DeadlineExceeded):
		return connect.NewError(connect.CodeDeadlineExceeded, err)
	case errors.Is(err, ErrInvalid):
		return connect.NewError(connect.CodeInvalidArgument, err)
	case errors.Is(err, ErrNotPermitted), errors.Is(err, os.ErrPermission):
		return connect.NewError(connect.CodePermissionDenied, err)
	case errors.Is(err, ErrUnavailable):
		return connect.NewError(connect.CodeUnavailable, err)
	case errors.Is(err, ErrRevisionConflict):
		return connect.NewError(connect.CodeAborted, err)
	case errors.Is(err, ErrExists):
		return connect.NewError(connect.CodeAlreadyExists, err)
	case errors.Is(err, os.ErrNotExist):
		return connect.NewError(connect.CodeNotFound, err)
	default:
		return connect.NewError(connect.CodeInternal, err)
	}
}
