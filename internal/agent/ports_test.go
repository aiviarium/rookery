package agent_test

import (
	"context"
	"errors"
	"io"
	"net"
	"path/filepath"
	"testing"
	"time"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/agent"
)

// wantCode checks that err is a connect error with a code.
func wantCode(t *testing.T, err error, code connect.Code) {
	t.Helper()

	if err == nil {
		t.Fatalf("the call succeeded, want %v", code)
	}
	var connectErr *connect.Error
	if !errors.As(err, &connectErr) || connectErr.Code() != code {
		t.Fatalf("the call failed with %v, want %v", err, code)
	}
}

func TestPorts(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialRoot(t)

	t.Run("loopback port", func(t *testing.T) {
		t.Parallel()

		listener, err := (&net.ListenConfig{}).Listen(context.Background(), "tcp", "127.0.0.1:0")
		if err != nil {
			t.Fatalf("listening: %v", err)
		}
		defer func() { _ = listener.Close() }()
		go echoOnce(listener)

		port := listener.Addr().(*net.TCPAddr).Port
		dialPort(t, client, &hostv1.PortServiceDialRequest_TcpPort{TcpPort: uint32(port)}, "ping") //nolint:gosec // a port is positive.
	})

	t.Run("unix socket", func(t *testing.T) {
		t.Parallel()

		path := filepath.Join(t.TempDir(), "echo.sock")
		listener, err := (&net.ListenConfig{}).Listen(context.Background(), "unix", path)
		if err != nil {
			t.Fatalf("listening: %v", err)
		}
		defer func() { _ = listener.Close() }()
		go echoOnce(listener)

		dialPort(t, client, &hostv1.PortServiceDialRequest_UnixPath{UnixPath: path}, "pong")
	})

	t.Run("nothing listening", func(t *testing.T) {
		t.Parallel()

		closed, err := (&net.ListenConfig{}).Listen(context.Background(), "tcp", "127.0.0.1:0")
		if err != nil {
			t.Fatalf("listening: %v", err)
		}
		port := closed.Addr().(*net.TCPAddr).Port
		_ = closed.Close()

		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		stream := client.Ports.Dial(ctx)
		if err := stream.Send(&hostv1.PortServiceDialRequest{
			Target: &hostv1.PortServiceDialRequest_TcpPort{TcpPort: uint32(port)}, //nolint:gosec // a port is positive.
		}); err != nil {
			t.Fatalf("sending the target: %v", err)
		}
		if _, err := stream.Receive(); err == nil {
			t.Fatal("a port with nothing behind it answered")
		}
	})
}

// dialPort sends data to a port through a Dial stream and checks what comes
// back.
func dialPort(t *testing.T, client *agent.Client, target any, data string) {
	t.Helper()

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	stream := client.Ports.Dial(ctx)
	first := &hostv1.PortServiceDialRequest{Data: []byte(data)}
	switch typed := target.(type) {
	case *hostv1.PortServiceDialRequest_TcpPort:
		first.Target = typed
	case *hostv1.PortServiceDialRequest_UnixPath:
		first.Target = typed
	default:
		t.Fatalf("unknown target %T", target)
	}
	if err := stream.Send(first); err != nil {
		t.Fatalf("sending the target: %v", err)
	}

	var received []byte
	for len(received) < len(data) {
		response, err := stream.Receive()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			t.Fatalf("receiving: %v", err)
		}
		received = append(received, response.GetData()...)
	}
	if string(received) != data {
		t.Errorf("the port answered %q, want %q", received, data)
	}
	if err := stream.CloseRequest(); err != nil {
		t.Fatalf("closing the request: %v", err)
	}
}

// echoOnce accepts one connection and sends back what it is given, until the
// other side is done.
func echoOnce(listener net.Listener) {
	conn, err := listener.Accept()
	if err != nil {
		return
	}
	defer func() { _ = conn.Close() }()

	buffer := make([]byte, 4096)
	for {
		read, err := conn.Read(buffer)
		if read > 0 {
			if _, writeErr := conn.Write(buffer[:read]); writeErr != nil {
				return
			}
		}
		if err != nil {
			return
		}
	}
}

// TestPortsRefusedOnUserSocket checks that ports are the server's: a local
// program connects to its own loopback without a daemon.
func TestPortsRefusedOnUserSocket(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialUser(t)

	stream := client.Ports.Dial(context.Background())
	if err := stream.Send(&hostv1.PortServiceDialRequest{
		Target: &hostv1.PortServiceDialRequest_TcpPort{TcpPort: 9},
	}); err != nil {
		t.Fatalf("sending the target: %v", err)
	}
	_, err := stream.Receive()
	wantCode(t, err, connect.CodePermissionDenied)
}
