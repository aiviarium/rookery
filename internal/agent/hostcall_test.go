package agent_test

import (
	"context"
	"errors"
	"testing"
	"time"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/agent"
)

// provider is a test's host service: it answers calls by echoing the arguments.
type provider struct {
	stream *connect.BidiStreamForClient[hostv1.HostCallServiceRegisterRequest, hostv1.HostCallServiceRegisterResponse]
	failed chan error
}

// serveHostService registers a service that echoes its arguments.
func serveHostService(t *testing.T, client *agent.Client, name string, methods ...string) *provider {
	t.Helper()

	stream := client.HostCalls.Register(context.Background())
	if err := stream.Send(&hostv1.HostCallServiceRegisterRequest{
		Message: &hostv1.HostCallServiceRegisterRequest_Service{Service: &hostv1.HostService{
			Name: name, Methods: methods,
		}},
	}); err != nil {
		t.Fatalf("registering %s: %v", name, err)
	}

	served := &provider{stream: stream, failed: make(chan error, 1)}
	go func() {
		for {
			message, err := stream.Receive()
			if err != nil {
				served.failed <- err
				return
			}
			call := message.GetRequest()
			if call == nil {
				continue
			}
			answer := &hostv1.HostCallServiceRegisterRequest{
				Message: &hostv1.HostCallServiceRegisterRequest_Result{Result: &hostv1.HostCallResult{
					Id: call.GetId(), ResultCbor: call.GetArgsCbor(),
				}},
			}
			if err := stream.Send(answer); err != nil {
				served.failed <- err
				return
			}
		}
	}()
	t.Cleanup(func() { _ = stream.CloseRequest() })
	return served
}

// registered waits for a service to appear in the listing.
func (p *provider) registered(t *testing.T, client *agent.Client, name string) {
	t.Helper()

	deadline := time.Now().Add(20 * time.Second)
	for time.Now().Before(deadline) {
		response, err := client.HostCalls.List(context.Background(), connect.NewRequest(&hostv1.HostCallServiceListRequest{}))
		if err != nil {
			t.Fatalf("listing host services: %v", err)
		}
		for _, service := range response.Msg.GetServices() {
			if service.GetName() == name {
				return
			}
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("%s did not register", name)
}

func TestHostServices(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialRoot(t)

	empty, err := client.HostCalls.List(context.Background(), connect.NewRequest(&hostv1.HostCallServiceListRequest{}))
	if err != nil {
		t.Fatalf("listing: %v", err)
	}
	if len(empty.Msg.GetServices()) != 0 {
		t.Fatalf("a fresh daemon has %v registered", empty.Msg.GetServices())
	}

	provider := serveHostService(t, client, "example/hello", "greet", "wave")
	provider.registered(t, client, "example/hello")

	answer, err := client.HostCalls.Call(context.Background(), connect.NewRequest(&hostv1.HostCallServiceCallRequest{
		Service: "example/hello", Method: "greet", ArgsCbor: []byte("arguments"),
	}))
	if err != nil {
		t.Fatalf("calling: %v", err)
	}
	if string(answer.Msg.GetResultCbor()) != "arguments" {
		t.Errorf("the answer is %q, want the arguments back", answer.Msg.GetResultCbor())
	}

	_, err = client.HostCalls.Call(context.Background(), connect.NewRequest(&hostv1.HostCallServiceCallRequest{
		Service: "example/hello", Method: "unknown",
	}))
	wantCode(t, err, connect.CodeNotFound)

	_, err = client.HostCalls.Call(context.Background(), connect.NewRequest(&hostv1.HostCallServiceCallRequest{
		Service: "example/nothing", Method: "greet",
	}))
	wantCode(t, err, connect.CodeUnavailable)

	if err := provider.stream.CloseRequest(); err != nil {
		t.Fatalf("unregistering: %v", err)
	}
	// The end of the stream and the registry's noticing it are two moments: a
	// call in flight while the provider goes away fails with the registry's own
	// error, and the next one finds nothing registered.
	deadline := time.Now().Add(20 * time.Second)
	for {
		_, err = client.HostCalls.Call(context.Background(), connect.NewRequest(&hostv1.HostCallServiceCallRequest{
			Service: "example/hello", Method: "greet",
		}))
		if err != nil {
			var connectErr *connect.Error
			if !errors.As(err, &connectErr) || connectErr.Code() != connect.CodeUnavailable {
				if time.Now().After(deadline) {
					t.Fatalf("a closed registration answers with %v", err)
				}
				time.Sleep(5 * time.Millisecond)
				continue
			}
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("a closed registration still answers")
		}
		time.Sleep(5 * time.Millisecond)
	}

	// A service that goes away while a call waits fails that call rather than
	// leaving it hanging. Registering twice is refused, which is the other half
	// of the same rule.
	serveHostService(t, client, "example/hello", "greet").registered(t, client, "example/hello")

	duplicate := client.HostCalls.Register(context.Background())
	if err := duplicate.Send(&hostv1.HostCallServiceRegisterRequest{
		Message: &hostv1.HostCallServiceRegisterRequest_Service{Service: &hostv1.HostService{Name: "example/hello"}},
	}); err != nil {
		t.Fatalf("registering twice: %v", err)
	}
	if _, err := duplicate.Receive(); err == nil {
		t.Error("a second registration of one name was accepted")
	} else {
		wantCode(t, err, connect.CodeAlreadyExists)
	}
}

// TestHostServiceFromUserSocket checks that a local program registers and calls
// host services too.
func TestHostServiceFromUserSocket(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	local := daemon.dialUser(t)
	root := daemon.dialRoot(t)

	provider := serveHostService(t, local, "example/local", "ping")
	provider.registered(t, root, "example/local")

	answer, err := root.HostCalls.Call(context.Background(), connect.NewRequest(&hostv1.HostCallServiceCallRequest{
		Service: "example/local", Method: "ping", ArgsCbor: []byte("pong?"),
	}))
	if err != nil {
		t.Fatalf("calling: %v", err)
	}
	if string(answer.Msg.GetResultCbor()) != "pong?" {
		t.Errorf("the answer is %q", answer.Msg.GetResultCbor())
	}
}

// TestHostServiceRepeatable checks that the same service may register again once
// its first stream has ended.
func TestHostServiceRepeatable(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialRoot(t)

	first := serveHostService(t, client, "example/repeat")
	first.registered(t, client, "example/repeat")
	if err := first.stream.CloseRequest(); err != nil {
		t.Fatalf("closing: %v", err)
	}

	second := serveHostService(t, client, "example/repeat")
	second.registered(t, client, "example/repeat")
}
