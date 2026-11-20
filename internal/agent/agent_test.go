package agent_test

import (
	"context"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"testing"
	"time"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/host/v1/hostv1connect"
	"github.com/aiviarium/rookery/internal/agent"
	"github.com/aiviarium/rookery/internal/pipeconn"
)

// daemon is a rookeryd a test runs: its two sockets, and clients for them.
type daemon struct {
	server *agent.Server
	root   string
	user   string
}

// startDaemon runs a daemon on two sockets in a short temporary directory, as a
// test's user, and stops it when the test ends.
func startDaemon(t *testing.T, configure func(*agent.Config)) *daemon {
	t.Helper()

	dir := t.TempDir()
	config := agent.Config{
		Version:        "test",
		RootSocket:     filepath.Join(dir, "root.sock"),
		UserSocket:     filepath.Join(dir, "user.sock"),
		ChannelDB:      filepath.Join(dir, "channel.db"),
		ActionsPath:    filepath.Join(dir, "actions.json"),
		DesktopEnvPath: filepath.Join(dir, "desktop.env"),
		Log:            slog.New(slog.DiscardHandler),
	}
	if configure != nil {
		configure(&config)
	}

	server, err := agent.New(context.Background(), config)
	if err != nil {
		t.Fatalf("starting the daemon: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	served := make(chan error, 1)
	go func() { served <- server.Serve(ctx) }()
	t.Cleanup(func() {
		cancel()
		select {
		case err := <-served:
			if err != nil {
				t.Errorf("stopping the daemon: %v", err)
			}
		case <-time.After(10 * time.Second):
			t.Error("the daemon did not stop")
		}
	})

	return &daemon{server: server, root: config.RootSocket, user: config.UserSocket}
}

// dialRoot and dialUser make clients for the two sockets.
func (d *daemon) dialRoot(t *testing.T) *agent.Client {
	t.Helper()
	client, err := agent.Dial(d.root)
	if err != nil {
		t.Fatalf("dialing the root socket: %v", err)
	}
	t.Cleanup(func() { _ = client.Close() })
	return client
}

func (d *daemon) dialUser(t *testing.T) *agent.Client {
	t.Helper()
	client, err := agent.Dial(d.user)
	if err != nil {
		t.Fatalf("dialing the user socket: %v", err)
	}
	t.Cleanup(func() { _ = client.Close() })
	return client
}

// TestSockets checks that both sockets are there with the modes the plan gives
// them: the server's is root's alone, the local one is everyone's.
func TestSockets(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	for name, want := range map[string]os.FileMode{
		daemon.root: 0o600,
		daemon.user: 0o666,
	} {
		info, err := os.Stat(name)
		if err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		if info.Mode().Perm() != want {
			t.Errorf("%s has mode %o, want %o", name, info.Mode().Perm(), want)
		}
	}
}

// TestVersion checks the daemon's own answer on both sockets.
func TestVersion(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	for name, client := range map[string]*agent.Client{
		"root": daemon.dialRoot(t),
		"user": daemon.dialUser(t),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			response, err := client.Agent.Version(context.Background(), connect.NewRequest(&hostv1.AgentServiceVersionRequest{}))
			if err != nil {
				t.Fatalf("Version: %v", err)
			}
			if response.Msg.GetVersion() != "test" {
				t.Errorf("Version = %q, want test", response.Msg.GetVersion())
			}
			if response.Msg.GetOs() == "" || response.Msg.GetArch() == "" {
				t.Errorf("Version did not report the platform: %+v", response.Msg)
			}
			if response.Msg.GetHostname() == "" {
				t.Error("Version did not report the host name")
			}
		})
	}
}

// TestConnectBridge puts a client on the daemon's root socket through
// agent.Connect, which is what `rookeryd connect` does for the server's SSH
// session.
func TestConnectBridge(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	toDaemonReader, toDaemonWriter := io.Pipe()
	fromDaemonReader, fromDaemonWriter := io.Pipe()
	bridge := make(chan error, 1)
	go func() { bridge <- agent.Connect(ctx, daemon.root, toDaemonReader, fromDaemonWriter) }()

	conn, err := pipeconn.New(pipeconn.Pair{Reader: fromDaemonReader, Writer: toDaemonWriter})
	if err != nil {
		t.Fatalf("building the pipe: %v", err)
	}
	client := hostv1connect.NewAgentServiceClient(pipeconn.Client(conn), "http://rookeryd", connect.WithGRPC())

	response, err := client.Version(context.Background(), connect.NewRequest(&hostv1.AgentServiceVersionRequest{}))
	if err != nil {
		t.Fatalf("Version through the bridge: %v", err)
	}
	if response.Msg.GetVersion() != "test" {
		t.Errorf("Version = %q, want test", response.Msg.GetVersion())
	}
}
