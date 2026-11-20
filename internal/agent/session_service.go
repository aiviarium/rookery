package agent

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
)

// sessionService serves SessionService: terminals on the host.
type sessionService struct {
	server *Server
}

func (h *sessionService) Create(ctx context.Context, request *connect.Request[hostv1.SessionServiceCreateRequest]) (*connect.Response[hostv1.SessionServiceCreateResponse], error) {
	acct, _, err := accountFor(ctx, request.Msg.GetUser().GetName())
	if err != nil {
		return nil, err
	}

	sess, err := startSession(ctx, acct, request.Msg)
	if err != nil {
		return nil, toConnect(err)
	}
	h.server.sessions.add(sess)
	h.server.log.InfoContext(ctx, "opened a session",
		"session", sess.id, "user", acct.Name, "command", sess.command, "pid", sess.cmd.Process.Pid)

	return connect.NewResponse(&hostv1.SessionServiceCreateResponse{Session: sess.info()}), nil
}

func (h *sessionService) List(ctx context.Context, _ *connect.Request[hostv1.SessionServiceListRequest]) (*connect.Response[hostv1.SessionServiceListResponse], error) {
	caller, err := callerOf(ctx)
	if err != nil {
		return nil, err
	}

	owner := caller.account
	if caller.role == roleRoot {
		owner = nil
	}
	sessions := h.server.sessions.list(owner)
	response := &hostv1.SessionServiceListResponse{Sessions: make([]*hostv1.Session, 0, len(sessions))}
	for _, sess := range sessions {
		response.Sessions = append(response.Sessions, sess.info())
	}
	return connect.NewResponse(response), nil
}

// own returns a session the caller may act on. Another account's session is not
// found rather than refused, so that a caller cannot learn what it may not see.
func (h *sessionService) own(caller *caller, id string) (*session, error) {
	sess, err := h.server.sessions.get(id)
	if err != nil {
		return nil, toConnect(err)
	}
	if caller.role == roleUser && sess.account.Name != caller.account.Name {
		return nil, toConnect(fmt.Errorf("%w: no session %s", os.ErrNotExist, id))
	}
	return sess, nil
}

func (h *sessionService) Attach(ctx context.Context, stream *connect.BidiStream[hostv1.SessionServiceAttachRequest, hostv1.SessionServiceAttachResponse]) error {
	first, err := stream.Receive()
	if errors.Is(err, io.EOF) {
		return nil
	}
	if err != nil {
		return err
	}

	caller, err := callerOf(ctx)
	if err != nil {
		return err
	}
	sess, err := h.own(caller, first.GetId())
	if err != nil {
		return err
	}
	if first.GetCols() != 0 || first.GetRows() != 0 {
		if err := sess.resize(first.GetCols(), first.GetRows()); err != nil {
			return toConnect(err)
		}
	}

	viewer, replay, from, truncated := sess.attach(first.GetOffset())
	defer sess.detach(viewer)

	if len(replay) > 0 || truncated {
		response := &hostv1.SessionServiceAttachResponse{Output: replay, Offset: from, Truncated: truncated}
		if err := stream.Send(response); err != nil {
			return err
		}
	}
	replayed := from + uint64(len(replay))
	if len(first.GetInput()) > 0 {
		if err := sess.write(first.GetInput()); err != nil {
			return toConnect(err)
		}
	}

	// One goroutine sends and one receives, because a stream is one way at each
	// end. Whichever ends first ends the handler. The sender is stopped and
	// waited for before the handler returns: a message written after a handler
	// has finished is a panic in net/http.
	ended := make(chan error, 2)
	stop := make(chan struct{})
	sent := make(chan struct{})
	go func() {
		defer close(sent)
		ended <- sendSessionOutput(stream, sess, viewer, replayed, stop)
	}()
	go func() { ended <- readSessionInput(stream, sess) }()

	err = <-ended
	close(stop)
	// The sender is waited for even when the call is over: a message written
	// after a handler has returned is a panic in net/http, and a canceled stream
	// ends the sender's own Send.
	<-sent
	return err
}

// sendSessionOutput streams a session's output to a viewer until the session
// ends, the caller goes, or the handler stops it.
func sendSessionOutput(stream *connect.BidiStream[hostv1.SessionServiceAttachRequest, hostv1.SessionServiceAttachResponse], sess *session, viewer *viewer, replayed uint64, stop <-chan struct{}) error {
	for {
		var output chunk
		var open bool
		select {
		case <-stop:
			return nil
		case output, open = <-viewer.chunks:
			if !open {
				if sess.viewerDropped(viewer) {
					return stream.Send(&hostv1.SessionServiceAttachResponse{Dropped: true})
				}
				if finished, code := sess.exitStatus(); finished {
					return stream.Send(&hostv1.SessionServiceAttachResponse{Exited: true, ExitCode: int32(code)}) //nolint:gosec // an exit code is small.
				}
				return nil
			}
		}

		if output.offset+uint64(len(output.data)) <= replayed {
			continue
		}
		if output.offset < replayed {
			output.data = output.data[replayed-output.offset:]
			output.offset = replayed
		}
		if err := stream.Send(&hostv1.SessionServiceAttachResponse{Output: output.data, Offset: output.offset}); err != nil {
			return err
		}
	}
}

func readSessionInput(stream *connect.BidiStream[hostv1.SessionServiceAttachRequest, hostv1.SessionServiceAttachResponse], sess *session) error {
	for {
		request, err := stream.Receive()
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return err
		}
		if request.GetCols() != 0 || request.GetRows() != 0 {
			if err := sess.resize(request.GetCols(), request.GetRows()); err != nil {
				return toConnect(err)
			}
		}
		if len(request.GetInput()) > 0 {
			if err := sess.write(request.GetInput()); err != nil {
				return toConnect(err)
			}
		}
	}
}

func (h *sessionService) Resize(ctx context.Context, request *connect.Request[hostv1.SessionServiceResizeRequest]) (*connect.Response[hostv1.SessionServiceResizeResponse], error) {
	caller, err := callerOf(ctx)
	if err != nil {
		return nil, err
	}
	sess, err := h.own(caller, request.Msg.GetId())
	if err != nil {
		return nil, err
	}
	if err := sess.resize(request.Msg.GetCols(), request.Msg.GetRows()); err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&hostv1.SessionServiceResizeResponse{}), nil
}

func (h *sessionService) Signal(ctx context.Context, request *connect.Request[hostv1.SessionServiceSignalRequest]) (*connect.Response[hostv1.SessionServiceSignalResponse], error) {
	caller, err := callerOf(ctx)
	if err != nil {
		return nil, err
	}
	sess, err := h.own(caller, request.Msg.GetId())
	if err != nil {
		return nil, err
	}
	if err := sess.signal(request.Msg.GetSignal()); err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&hostv1.SessionServiceSignalResponse{}), nil
}

func (h *sessionService) Close(ctx context.Context, request *connect.Request[hostv1.SessionServiceCloseRequest]) (*connect.Response[hostv1.SessionServiceCloseResponse], error) {
	caller, err := callerOf(ctx)
	if err != nil {
		return nil, err
	}
	sess, err := h.own(caller, request.Msg.GetId())
	if err != nil {
		return nil, err
	}
	if err := sess.close(); err != nil {
		return nil, toConnect(err)
	}
	h.server.sessions.remove(sess.id)
	h.server.log.InfoContext(ctx, "closed a session", "session", sess.id, "user", sess.account.Name)
	return connect.NewResponse(&hostv1.SessionServiceCloseResponse{}), nil
}
