package boxcli

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os/exec"
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
		value = cborx.ParseText(args[2])
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
	return writeJSON(options.Stdout, cborx.JSONValue(answer))
}

// runServe offers a host service, running a command for each call.
//
// The command is run as `COMMAND METHOD` with the call's argument as JSON
// on stdin and its answer as JSON on stdout.
func runServe(ctx context.Context, options Options, args []string) error {
	name, command, err := splitServe(args)
	if err != nil {
		return err
	}

	service := &hostv1.HostService{Name: name, Description: "served by box on this host"}
	return serveHostService(ctx, options, service, commandHandler(options, command))
}

// splitServe reads `SERVICE -- COMMAND…`.
func splitServe(args []string) (string, []string, error) {
	if len(args) < 3 || args[1] != "--" {
		return "", nil, usage("usage: box serve SERVICE -- COMMAND…")
	}
	return args[0], args[2:], nil
}

// commandHandler runs a served command for a call.
func commandHandler(options Options, command []string) handlerFunc {
	return func(ctx context.Context, method string, args []byte) ([]byte, error) {
		var value any
		if len(args) > 0 {
			if err := cborx.Unmarshal(args, &value); err != nil {
				return nil, fmt.Errorf("box: decoding the service request: %w", err)
			}
		}
		var stdin, stdout bytes.Buffer
		if err := writeJSON(&stdin, jsonable(value)); err != nil {
			return nil, err
		}

		arguments := append(append([]string(nil), command[1:]...), method)
		cmd := exec.CommandContext(ctx, command[0], arguments...) //nolint:gosec // the unit's configured command.
		cmd.Env = options.Environ
		cmd.Stdin = &stdin
		cmd.Stdout = &stdout
		cmd.WaitDelay = time.Second
		if err := cmd.Run(); err != nil {
			return nil, fmt.Errorf("box: the service command failed: %w", err)
		}
		answer, err := readJSON(&stdout)
		if err != nil && !errors.Is(err, io.EOF) {
			return nil, fmt.Errorf("box: the service command must return one JSON value: %w", err)
		}
		return cborx.Marshal(answer)
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
		if code := connect.CodeOf(err); code == connect.CodeInvalidArgument || code == connect.CodePermissionDenied {
			return err
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
	client, err := dial(options)
	if err != nil {
		return err
	}
	defer func() { _ = client.Close() }()

	ctx, cancel := context.WithCancel(ctx)
	stream := client.HostCalls.Register(ctx)
	if err := stream.Send(&hostv1.HostCallServiceRegisterRequest{
		Message: &hostv1.HostCallServiceRegisterRequest_Service{Service: service},
	}); err != nil && !errors.Is(err, io.EOF) {
		cancel()
		_ = stream.CloseRequest()
		_ = stream.CloseResponse()
		return err
	}
	var calls sync.WaitGroup
	var sendMu sync.Mutex
	var pendingMu sync.Mutex
	pending := make(map[string]context.CancelFunc)
	requestClosed := make(chan struct{})
	// The HTTP/2 reader can remain blocked until the request body closes.
	// Serialize CloseRequest with sends, and wait for both sides before returning.
	stop := context.AfterFunc(ctx, func() {
		sendMu.Lock()
		defer sendMu.Unlock()
		defer close(requestClosed)
		_ = stream.CloseRequest()
	})
	defer func() {
		cancel()
		if stop() {
			sendMu.Lock()
			_ = stream.CloseRequest()
			sendMu.Unlock()
		} else {
			<-requestClosed
		}
		calls.Wait()
		_ = stream.CloseResponse()
	}()

	message, err := stream.Receive()
	if err != nil {
		return err
	}
	if !message.GetRegistered() {
		return errors.New("box: the daemon did not acknowledge the service")
	}
	options.logf("box: %s is registered", service.GetName())

	for {
		message, err := stream.Receive()
		if err != nil {
			return err
		}
		if id := message.GetCancelCallId(); id != "" {
			pendingMu.Lock()
			interrupt := pending[id]
			pendingMu.Unlock()
			if interrupt != nil {
				interrupt()
			}
			continue
		}
		if call := message.GetRequest(); call != nil {
			callCtx, finish := context.WithCancel(ctx)
			pendingMu.Lock()
			pending[call.GetId()] = finish
			pendingMu.Unlock()
			calls.Go(func() {
				defer func() {
					finish()
					pendingMu.Lock()
					delete(pending, call.GetId())
					pendingMu.Unlock()
				}()
				answer := &hostv1.HostCallResult{Id: call.GetId()}
				result, err := handler(callCtx, call.GetMethod(), call.GetArgsCbor())
				if callCtx.Err() != nil {
					return
				}
				if err != nil {
					answer.Error = err.Error()
				} else {
					answer.ResultCbor = result
				}
				sendMu.Lock()
				defer sendMu.Unlock()
				if callCtx.Err() != nil {
					return
				}
				if err := stream.Send(&hostv1.HostCallServiceRegisterRequest{
					Message: &hostv1.HostCallServiceRegisterRequest_Result{Result: answer},
				}); err != nil {
					cancel()
				}
			})
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
