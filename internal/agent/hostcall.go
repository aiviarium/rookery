package agent

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"sort"
	"strconv"
	"sync"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
)

// hostCallRegistry is the host services registered on the daemon: a program's
// offer of a named service with methods, and the calls waiting for an answer.
//
// A registration lasts as long as its stream. A call to a service that is not
// registered fails with unavailable, and a provider that goes away while a call
// waits gives that call an error rather than a hang.
type hostCallRegistry struct {
	mu       sync.Mutex
	services map[string]*hostService
	calls    uint64
}

// hostService is one registered service, and the calls it has not answered.
type hostService struct {
	info    *hostv1.HostService
	calls   chan *hostv1.HostCallRequest
	waiting map[string]chan *hostv1.HostCallResult
}

func newHostCallRegistry() *hostCallRegistry {
	return &hostCallRegistry{services: make(map[string]*hostService)}
}

// list returns the registered services, sorted by name.
func (r *hostCallRegistry) list() []*hostv1.HostService {
	r.mu.Lock()
	defer r.mu.Unlock()

	services := make([]*hostv1.HostService, 0, len(r.services))
	for _, service := range r.services {
		services = append(services, service.info)
	}
	sort.Slice(services, func(i, j int) bool { return services[i].GetName() < services[j].GetName() })
	return services
}

// register adds a service, for as long as its stream lasts.
func (r *hostCallRegistry) register(info *hostv1.HostService) (*hostService, error) {
	if info.GetName() == "" {
		return nil, fmt.Errorf("%w: a host service needs a name", ErrInvalid)
	}
	for _, method := range info.GetMethods() {
		if method == "" {
			return nil, fmt.Errorf("%w: a host service's method name is empty", ErrInvalid)
		}
	}

	r.mu.Lock()
	defer r.mu.Unlock()
	if _, taken := r.services[info.GetName()]; taken {
		return nil, fmt.Errorf("%w: %s is already registered", ErrExists, info.GetName())
	}

	service := &hostService{
		info:    info,
		calls:   make(chan *hostv1.HostCallRequest, 64),
		waiting: make(map[string]chan *hostv1.HostCallResult),
	}
	r.services[info.GetName()] = service
	return service, nil
}

// unregister removes a service and fails the calls it still owes.
func (r *hostCallRegistry) unregister(service *hostService) {
	r.mu.Lock()
	defer r.mu.Unlock()

	if r.services[service.info.GetName()] == service {
		delete(r.services, service.info.GetName())
	}
	close(service.calls)
	for id, waiting := range service.waiting {
		delete(service.waiting, id)
		waiting <- &hostv1.HostCallResult{Id: id, Error: "the provider went away"}
	}
}

// deliver hands a provider's answer to the call waiting for it.
func (r *hostCallRegistry) deliver(result *hostv1.HostCallResult) {
	r.mu.Lock()
	defer r.mu.Unlock()

	for _, service := range r.services {
		waiting, ok := service.waiting[result.GetId()]
		if !ok {
			continue
		}
		delete(service.waiting, result.GetId())
		waiting <- result
		return
	}
}

// call runs one method of a registered service and returns its answer.
func (r *hostCallRegistry) call(ctx context.Context, name, method string, args []byte) ([]byte, error) {
	r.mu.Lock()
	service, ok := r.services[name]
	if !ok {
		r.mu.Unlock()
		return nil, fmt.Errorf("%w: no host service %s is registered", ErrUnavailable, name)
	}
	if len(service.info.GetMethods()) > 0 && !hasMethod(service.info, method) {
		r.mu.Unlock()
		return nil, fmt.Errorf("%w: %s has no method %s", os.ErrNotExist, name, method)
	}

	r.calls++
	id := strconv.FormatUint(r.calls, 10)
	waiting := make(chan *hostv1.HostCallResult, 1)
	service.waiting[id] = waiting
	r.mu.Unlock()

	defer func() {
		r.mu.Lock()
		delete(service.waiting, id)
		r.mu.Unlock()
	}()

	call := &hostv1.HostCallRequest{Id: id, Method: method, ArgsCbor: args}
	select {
	case service.calls <- call:
	case <-ctx.Done():
		return nil, ctx.Err()
	}

	select {
	case result := <-waiting:
		if result.GetError() != "" {
			return nil, connect.NewError(connect.CodeInternal,
				fmt.Errorf("agent: %s.%s failed: %s", name, method, result.GetError()))
		}
		return result.GetResultCbor(), nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

// hasMethod says whether a service declares a method.
func hasMethod(info *hostv1.HostService, method string) bool {
	for _, declared := range info.GetMethods() {
		if declared == method {
			return true
		}
	}
	return false
}

// hostCallService serves HostCallService: the host services and their calls.
type hostCallService struct {
	server *Server
}

func (h *hostCallService) Register(ctx context.Context, stream *connect.BidiStream[hostv1.HostCallServiceRegisterRequest, hostv1.HostCallServiceRegisterResponse]) error {
	first, err := stream.Receive()
	if errors.Is(err, io.EOF) {
		return nil
	}
	if err != nil {
		return err
	}
	if first.GetService() == nil {
		return invalid("agent: a register stream's first message is its service")
	}

	service, err := h.server.registry.register(first.GetService())
	if err != nil {
		return toConnect(err)
	}
	defer h.server.registry.unregister(service)
	h.server.log.InfoContext(ctx, "a host service registered",
		"service", service.info.GetName(), "methods", service.info.GetMethods())

	// One goroutine sends calls and one receives answers, as a stream is one way
	// at each end. The sender is stopped and waited for before the handler
	// returns: a message after a handler has finished is a panic in net/http.
	ended := make(chan error, 2)
	stop := make(chan struct{})
	sent := make(chan struct{})
	go func() {
		defer close(sent)
		ended <- sendHostCalls(stream, service, stop)
	}()
	go func() { ended <- receiveHostResults(stream, h.server.registry) }()

	err = <-ended
	close(stop)
	<-sent
	return err
}

// sendHostCalls passes the registry's calls to the provider.
func sendHostCalls(stream *connect.BidiStream[hostv1.HostCallServiceRegisterRequest, hostv1.HostCallServiceRegisterResponse], service *hostService, stop <-chan struct{}) error {
	for {
		select {
		case <-stop:
			return nil
		case call, open := <-service.calls:
			if !open {
				return nil
			}
			if err := stream.Send(&hostv1.HostCallServiceRegisterResponse{
				Message: &hostv1.HostCallServiceRegisterResponse_Request{Request: call},
			}); err != nil {
				return err
			}
		}
	}
}

// receiveHostResults takes the provider's answers and hands them to the calls
// waiting for them.
func receiveHostResults(stream *connect.BidiStream[hostv1.HostCallServiceRegisterRequest, hostv1.HostCallServiceRegisterResponse], registry *hostCallRegistry) error {
	for {
		message, err := stream.Receive()
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return err
		}
		if result := message.GetResult(); result != nil {
			registry.deliver(result)
		}
	}
}

func (h *hostCallService) List(ctx context.Context, _ *connect.Request[hostv1.HostCallServiceListRequest]) (*connect.Response[hostv1.HostCallServiceListResponse], error) {
	if _, err := callerOf(ctx); err != nil {
		return nil, err
	}
	return connect.NewResponse(&hostv1.HostCallServiceListResponse{Services: h.server.registry.list()}), nil
}

func (h *hostCallService) Call(ctx context.Context, request *connect.Request[hostv1.HostCallServiceCallRequest]) (*connect.Response[hostv1.HostCallServiceCallResponse], error) {
	if _, err := callerOf(ctx); err != nil {
		return nil, err
	}
	result, err := h.server.registry.call(ctx, request.Msg.GetService(), request.Msg.GetMethod(), request.Msg.GetArgsCbor())
	if err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&hostv1.HostCallServiceCallResponse{ResultCbor: result}), nil
}
