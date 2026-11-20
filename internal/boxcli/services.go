package boxcli

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"os/exec"
	"strings"
	"sync"
	"time"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/agent"
	"github.com/aiviarium/rookery/internal/cborx"
)

// runServices lists the host services registered on the host.
func runServices(ctx context.Context, options Options, args []string) error {
	if len(args) != 0 {
		return usage("usage: box services")
	}

	client, err := dial(options)
	if err != nil {
		return err
	}
	defer func() { _ = client.Close() }()

	response, err := client.HostCalls.List(ctx, connect.NewRequest(&hostv1.HostCallServiceListRequest{}))
	if err != nil {
		return connectError("services", err)
	}

	services := make([]map[string]any, 0, len(response.Msg.GetServices()))
	for _, service := range response.Msg.GetServices() {
		services = append(services, map[string]any{
			"name":        service.GetName(),
			"methods":     service.GetMethods(),
			"description": service.GetDescription(),
		})
	}
	return writeJSON(options.Stdout, services)
}

// runCall calls one method of a host service and prints its answer.
func runCall(ctx context.Context, options Options, args []string) error {
	if len(args) < 2 || len(args) > 3 {
		return usage("usage: box call SERVICE METHOD [VALUE]")
	}

	var value any
	if len(args) == 3 {
		value = parseValue(args[2])
	}
	arguments, err := cborx.Marshal(value)
	if err != nil {
		return fmt.Errorf("box: encoding the arguments: %w", err)
	}

	client, err := dial(options)
	if err != nil {
		return err
	}
	defer func() { _ = client.Close() }()

	response, err := client.HostCalls.Call(ctx, connect.NewRequest(&hostv1.HostCallServiceCallRequest{
		Service: args[0], Method: args[1], ArgsCbor: arguments,
	}))
	if err != nil {
		return connectError("call", err)
	}

	var answer any
	if encoded := response.Msg.GetResultCbor(); len(encoded) > 0 {
		if err := cborx.Unmarshal(encoded, &answer); err != nil {
			return fmt.Errorf("box: decoding the answer: %w", err)
		}
	}
	return writeJSON(options.Stdout, jsonable(answer))
}

// runServe offers a host service, running a command for each call.
//
// The command is run as `COMMAND METHOD` with the call's arguments as CBOR on
// its standard input and its answer as CBOR on its standard output. Calls are run
// one at a time, so a service that takes a while holds its own queue and nothing
// else.
func runServe(ctx context.Context, options Options, args []string) error {
	name, command, err := splitServe(args)
	if err != nil {
		return err
	}

	service := &hostv1.HostService{Name: name, Description: "served by box on this host"}
	handler := commandHandler(ctx, options, command)
	if err := serveHostService(ctx, options, service, handler); err != nil {
		return err
	}
	return nil
}

// splitServe reads `SERVICE -- COMMAND…`.
func splitServe(args []string) (string, []string, error) {
	if len(args) < 3 || args[1] != "--" {
		return "", nil, usage("usage: box serve SERVICE -- COMMAND…")
	}
	return args[0], args[2:], nil
}

// commandHandler runs a served command for a call.
func commandHandler(_ context.Context, options Options, command []string) handlerFunc {
	return func(ctx context.Context, method string, args []byte) ([]byte, error) {
		cmd := exec.CommandContext(ctx, command[0], append(command[1:], method)...) //nolint:gosec // the unit's command, on purpose.
		cmd.Env = options.Environ
		cmd.Stdin = bytes.NewReader(args)

		var stdout, stderr bytes.Buffer
		cmd.Stdout = &stdout
		cmd.Stderr = &stderr
		if err := cmd.Run(); err != nil {
			if message := strings.TrimSpace(stderr.String()); message != "" {
				return nil, fmt.Errorf("%s %s: %s: %w", command[0], method, message, err)
			}
			return nil, fmt.Errorf("%s %s: %w", command[0], method, err)
		}
		return stdout.Bytes(), nil
	}
}

// serveHostService registers a service and keeps it registered: when the
// daemon goes away and comes back, box registers again.
func serveHostService(ctx context.Context, options Options, service *hostv1.HostService, handler handlerFunc) error {
	backoff := time.Second
	for {
		err := registerOnce(ctx, options, service, handler)
		if ctx.Err() != nil {
			return nil
		}
		options.logf("box: %s is not registered: %v", service.GetName(), err)

		select {
		case <-ctx.Done():
			return nil
		case <-time.After(backoff):
		}
		if backoff < 10*time.Second {
			backoff *= 2
		}
	}
}

// registerOnce registers a service for the life of one stream.
func registerOnce(ctx context.Context, options Options, service *hostv1.HostService, handler handlerFunc) error {
	client, err := agent.Dial(options.Socket)
	if err != nil {
		return err
	}
	defer func() { _ = client.Close() }()

	stream := client.HostCalls.Register(ctx)
	if err := stream.Send(&hostv1.HostCallServiceRegisterRequest{
		Message: &hostv1.HostCallServiceRegisterRequest_Service{Service: service},
	}); err != nil {
		return err
	}
	options.logf("box: %s is registered", service.GetName())

	// The messages are read in a goroutine so that the daemon going away ends
	// this: connect's Receive can stay blocked after the call's context is
	// canceled while the request's own body is still open, and closing it is what
	// ends the read.
	// A call's handler may take a while, and a stream has one sender, so the
	// answers queue on a lock of their own.
	var (
		calls   sync.WaitGroup
		sendMux sync.Mutex
	)

	received := make(chan *hostv1.HostCallServiceRegisterResponse, 16)
	receiveErr := make(chan error, 1)
	go func() {
		for {
			message, err := stream.Receive()
			if err != nil {
				receiveErr <- err
				return
			}
			select {
			case received <- message:
			case <-ctx.Done():
				return
			}
		}
	}()

	for {
		select {
		case <-ctx.Done():
			_ = stream.CloseRequest()
			_ = stream.CloseResponse()
			calls.Wait()
			return nil
		case err := <-receiveErr:
			if ctx.Err() != nil {
				return nil
			}
			return err
		case message := <-received:
			call := message.GetRequest()
			if call == nil {
				continue
			}
			// Calls run at once: a paused desktop holds its own screenshot while
			// the resume that ends the pause arrives as another call. A stream
			// has one sender, so the answers take a turn.
			calls.Add(1)
			go func(call *hostv1.HostCallRequest) {
				defer calls.Done()
				answer := &hostv1.HostCallResult{Id: call.GetId()}
				result, callErr := handler(ctx, call.GetMethod(), call.GetArgsCbor())
				if callErr != nil {
					answer.Error = callErr.Error()
				} else {
					answer.ResultCbor = result
				}
				sendMux.Lock()
				defer sendMux.Unlock()
				_ = stream.Send(&hostv1.HostCallServiceRegisterRequest{
					Message: &hostv1.HostCallServiceRegisterRequest_Result{Result: answer},
				})
			}(call)
		}
	}
}

// handlerFunc runs one call of a served host service.
type handlerFunc func(ctx context.Context, method string, args []byte) ([]byte, error)

// dial makes a client for the daemon's socket for local programs.
func dial(options Options) (*agent.Client, error) {
	return agent.Dial(options.Socket)
}

// connectError says what a failed call means in box's terms.
func connectError(what string, err error) error {
	var connectErr *connect.Error
	if !errors.As(err, &connectErr) {
		return fmt.Errorf("box: %s: %w", what, err)
	}
	switch connectErr.Code() {
	case connect.CodeUnavailable:
		return fmt.Errorf("box: %s: rookeryd is not answering, or has nothing to offer: %w", what, err)
	case connect.CodePermissionDenied:
		return fmt.Errorf("box: %s: not for this account: %w", what, err)
	case connect.CodeNotFound:
		return fmt.Errorf("box: %s: there is no such thing: %w", what, err)
	default:
		return fmt.Errorf("box: %s: %w", what, err)
	}
}
