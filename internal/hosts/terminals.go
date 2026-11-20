package hosts

import (
	"context"
	"errors"
	"fmt"
	"io"
	"sort"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/proto"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/link"
)

// TerminalService exposes PTYs that live on the host daemon.
type TerminalService struct{ *Runtime }

type terminalRecord struct {
	host     string
	owner    string
	private  bool
	elevated bool
}

func terminalOn(host string, session *hostv1.Session) terminalRecord {
	sharing := session.GetSharing()
	return terminalRecord{
		host: host, owner: sharing.GetOwner(), private: sharing.GetPrivate(),
		elevated: sharing == nil || sharing.GetElevated(),
	}
}

func (r *Runtime) rememberTerminal(host string, session *hostv1.Session) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.byID[session.GetId()] = terminalOn(host, session)
}

func (r *Runtime) forgetTerminal(id, host string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.byID[id].host == host {
		delete(r.byID, id)
	}
}

func (r *Runtime) terminal(ctx context.Context, id string, ownerOnly bool) (terminalRecord, error) {
	r.mu.RLock()
	record, ok := r.byID[id]
	r.mu.RUnlock()
	if !ok {
		return terminalRecord{}, fmt.Errorf("%w: terminal %q", access.ErrNotFound, id)
	}
	return record, r.allowTerminal(ctx, record, ownerOnly)
}

func (r *Runtime) allowTerminal(ctx context.Context, record terminalRecord, ownerOnly bool) error {
	principal, _ := access.PrincipalFrom(ctx)
	need := access.Use
	if record.elevated || ((record.private || ownerOnly) && principal.String() != record.owner) {
		need = access.Manage
	}
	return r.hosts.deps.Checker.Check(ctx, principal, access.HostURI(record.host), need)
}

// Open starts a persistent PTY on a host.
func (s *TerminalService) Open(ctx context.Context, request *connect.Request[rookeryv1.TerminalServiceOpenRequest]) (*connect.Response[rookeryv1.TerminalServiceOpenResponse], error) {
	name, input := request.Msg.GetHost(), request.Msg.GetSession()
	record, err := s.hosts.authorizeHost(ctx, name, access.Use, CapabilityTerminal)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	user, elevated, err := s.account(ctx, record, input.GetUser().GetName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	remote := &hostv1.SessionServiceCreateRequest{}
	if input != nil {
		remote = proto.CloneOf(input)
	}
	remote.User = user
	remote.Sharing = &hostv1.SessionSharing{
		Owner: access.MustPrincipal(ctx).String(), Private: request.Msg.GetPrivate(), Elevated: elevated,
	}
	terminal, err := s.open(ctx, record.Name, remote)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.TerminalServiceOpenResponse{Terminal: terminal}), nil
}

func (s *TerminalService) open(ctx context.Context, host string, request *hostv1.SessionServiceCreateRequest) (*rookeryv1.Terminal, error) {
	client, release, err := s.hosts.deps.Links.Use(ctx, host)
	if err != nil {
		return nil, err
	}
	defer release()
	response, err := client.Sessions.Create(ctx, connect.NewRequest(request))
	if err != nil {
		return nil, err
	}
	session := response.Msg.GetSession()
	s.rememberTerminal(host, session)
	return &rookeryv1.Terminal{Host: host, Session: session}, nil
}

// List returns the host terminals the principal may attach to.
func (s *TerminalService) List(ctx context.Context, request *connect.Request[rookeryv1.TerminalServiceListRequest]) (*connect.Response[rookeryv1.TerminalServiceListResponse], error) {
	host := request.Msg.GetHost()
	client, release, err := s.use(ctx, host, access.Use, CapabilityTerminal)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	defer release()
	sessions, err := s.syncTerminals(ctx, host, client)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	sort.Slice(sessions, func(i, j int) bool { return sessions[i].GetId() < sessions[j].GetId() })
	result := &rookeryv1.TerminalServiceListResponse{}
	for _, session := range sessions {
		if err := s.allowTerminal(ctx, terminalOn(host, session), false); err == nil {
			result.Terminals = append(result.Terminals, &rookeryv1.Terminal{Host: host, Session: session})
		} else if !errors.Is(err, access.ErrDenied) {
			return nil, access.ToConnect(err)
		}
	}
	return connect.NewResponse(result), nil
}

func (r *Runtime) syncTerminals(ctx context.Context, host string, client *link.Client) ([]*hostv1.Session, error) {
	response, err := client.Sessions.List(ctx, connect.NewRequest(&hostv1.SessionServiceListRequest{}))
	if err != nil {
		return nil, err
	}
	sessions := response.Msg.GetSessions()
	for _, session := range sessions {
		r.rememberTerminal(host, session)
	}
	return sessions, nil
}

// Attach starts a terminal relay after checking the terminal's policy on its host.
func (s *TerminalService) Attach(ctx context.Context, stream *connect.BidiStream[rookeryv1.TerminalServiceAttachRequest, rookeryv1.TerminalServiceAttachResponse]) error {
	first, err := stream.Receive()
	if errors.Is(err, io.EOF) {
		return nil
	}
	if err != nil {
		return err
	}
	return access.ToConnect(s.relay(ctx, first.GetAttach(), func() (*hostv1.SessionServiceAttachRequest, error) {
		message, err := stream.Receive()
		return message.GetAttach(), err
	}, func(event *hostv1.SessionServiceAttachResponse) error {
		return stream.Send(&rookeryv1.TerminalServiceAttachResponse{Event: event})
	}))
}

// relay keeps writes to the inbound handler on its own goroutine. Canceling
// the upstream stream and returning releases a reader blocked on client input.
func (s *TerminalService) relay(ctx context.Context, first *hostv1.SessionServiceAttachRequest, receive func() (*hostv1.SessionServiceAttachRequest, error), send func(*hostv1.SessionServiceAttachResponse) error) error {
	record, err := s.terminal(ctx, first.GetId(), false)
	if err != nil {
		return err
	}
	client, release, err := s.hosts.deps.Links.Use(ctx, record.host)
	if err != nil {
		return err
	}
	defer release()
	upstreamCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	upstream := client.Sessions.Attach(upstreamCtx)
	if err := upstream.Send(first); err != nil && !errors.Is(err, io.EOF) {
		return err
	}
	inputErr := make(chan error, 1)
	go func() {
		for {
			message, err := receive()
			if errors.Is(err, io.EOF) {
				_ = upstream.CloseRequest()
				return
			}
			if err == nil {
				if message.GetId() != "" {
					err = fmt.Errorf("%w: only the first message identifies the terminal", access.ErrInvalid)
				} else {
					err = upstream.Send(message)
				}
			}
			if err != nil {
				if !errors.Is(err, io.EOF) {
					inputErr <- err
					cancel()
				}
				return
			}
		}
	}()
	for {
		event, err := upstream.Receive()
		if err != nil {
			select {
			case input := <-inputErr:
				return input
			default:
			}
			switch {
			case errors.Is(err, io.EOF):
				return nil
			case connect.CodeOf(err) == connect.CodeNotFound:
				s.forgetTerminal(first.GetId(), record.host)
			}
			return err
		}
		if err := send(event); err != nil {
			return err
		}
	}
}

// Resize changes the PTY size in characters.
func (s *TerminalService) Resize(ctx context.Context, request *connect.Request[rookeryv1.TerminalServiceResizeRequest]) (*connect.Response[rookeryv1.TerminalServiceResizeResponse], error) {
	id := request.Msg.GetId()
	record, err := s.terminal(ctx, id, false)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	client, release, err := s.hosts.deps.Links.Use(ctx, record.host)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	defer release()
	_, err = client.Sessions.Resize(ctx, connect.NewRequest(&hostv1.SessionServiceResizeRequest{
		Id: id, Cols: request.Msg.GetCols(), Rows: request.Msg.GetRows(),
	}))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.TerminalServiceResizeResponse{}), nil
}

// Close ends a terminal as its owner or the host's manager.
func (s *TerminalService) Close(ctx context.Context, request *connect.Request[rookeryv1.TerminalServiceCloseRequest]) (*connect.Response[rookeryv1.TerminalServiceCloseResponse], error) {
	id := request.Msg.GetId()
	record, err := s.terminal(ctx, id, true)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	client, release, err := s.hosts.deps.Links.Use(ctx, record.host)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	defer release()
	_, err = client.Sessions.Close(ctx, connect.NewRequest(&hostv1.SessionServiceCloseRequest{Id: id}))
	if err == nil || connect.CodeOf(err) == connect.CodeNotFound {
		s.forgetTerminal(id, record.host)
	}
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.TerminalServiceCloseResponse{}), nil
}
