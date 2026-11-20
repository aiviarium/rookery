//go:build e2e

package e2e_test

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"os"
	"os/exec"
	"os/user"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"connectrpc.com/connect"
	"github.com/jezek/xgb"
	"github.com/jezek/xgb/xproto"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/agent"
	"github.com/aiviarium/rookery/internal/boxcli"
	"github.com/aiviarium/rookery/internal/link/linktest"
	"github.com/aiviarium/rookery/internal/server"
)

func startServer(ctx context.Context, t *testing.T, databaseURL, releaseDir string) (string, string) {
	t.Helper()
	listener := loopback(ctx, t)
	address := listener.Addr().String()
	_ = listener.Close()
	dir := t.TempDir()
	socket := filepath.Join(dir, "rookery.sock")
	env := map[string]string{
		"ROOKERY_DATABASE_URL":  databaseURL,
		"ROOKERY_LISTEN":        address,
		"ROOKERY_SOCKET":        socket,
		"ROOKERY_STATE_DIR":     dir,
		"ROOKERY_COOKIE_SECURE": "false",
		"ROOKERY_RELEASE_DIR":   releaseDir,
	}
	config, err := server.LoadFrom(func(key string) string { return env[key] })
	if err != nil {
		t.Fatal(err)
	}
	config.Version = "e2e"
	site := config.PublicURL
	serveCtx, cancel := context.WithCancel(ctx)
	done := make(chan struct{})
	var runErr error
	go func() {
		defer close(done)
		runErr = server.New(config, slog.New(slog.DiscardHandler)).Run(serveCtx)
	}()
	t.Cleanup(func() {
		cancel()
		select {
		case <-done:
			if runErr != nil && !errors.Is(runErr, context.Canceled) {
				t.Errorf("server stopped: %v", runErr)
			}
		case <-time.After(5 * time.Second):
			t.Error("server failed to stop")
		}
	})
	if err := await(ctx, func() bool {
		select {
		case <-done:
			t.Fatalf("server stopped while starting: %v", runErr)
		default:
		}
		request, err := http.NewRequestWithContext(ctx, http.MethodGet, site+"/healthz", nil)
		if err != nil {
			t.Fatal(err)
		}
		response, err := http.DefaultClient.Do(request)
		if err != nil {
			return false
		}
		_ = response.Body.Close()
		return response.StatusCode == http.StatusOK
	}); err != nil {
		t.Fatal(err)
	}
	return site, socket
}

type hostFixture struct {
	ssh        *linktest.SSH
	runtime    string
	user       string
	userSocket string
}

func startHost(ctx context.Context, t *testing.T, display string) hostFixture {
	t.Helper()
	dir, err := os.MkdirTemp("", "rkh-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	current, err := user.Current()
	if err != nil {
		t.Fatal(err)
	}

	envPath := filepath.Join(dir, "desktop.env")
	if display != "" {
		env := fmt.Sprintf("DISPLAY=%s\nXDG_RUNTIME_DIR=%s\n", display, dir)
		if err := os.WriteFile(envPath, []byte(env), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	host, err := agent.New(ctx, agent.Config{
		Version:        "e2e",
		DesktopUser:    current.Username,
		Operator:       current.Username,
		DesktopEnvPath: envPath,
		ChannelDB:      filepath.Join(dir, "channel.db"),
		RootSocket:     filepath.Join(dir, "root.sock"),
		UserSocket:     filepath.Join(dir, "user.sock"),
		ActionsPath:    filepath.Join(dir, "actions.json"),
		SecretsDir:     filepath.Join(dir, "secrets"),
		Log:            slog.New(slog.DiscardHandler),
	})
	if err != nil {
		t.Fatal(err)
	}
	serveCtx, cancel := context.WithCancel(ctx)
	served := make(chan error, 1)
	go func() { served <- host.Serve(serveCtx) }()
	t.Cleanup(func() {
		cancel()
		if err := <-served; err != nil {
			t.Errorf("stopping rookeryd: %v", err)
		}
	})
	if display != "" {
		providerCtx, cancel := context.WithCancel(ctx)
		provider := make(chan error, 1)
		go func() {
			provider <- boxcli.Run(providerCtx, boxcli.Options{
				Socket: host.UserSocket(),
			}, []string{"desktop", "serve", "--display", display})
		}()
		t.Cleanup(func() {
			cancel()
			if err := <-provider; err != nil {
				t.Errorf("stopping the desktop provider: %v", err)
			}
		})
		client, err := agent.Dial(host.UserSocket())
		if err != nil {
			t.Fatal(err)
		}
		defer func() { _ = client.Close() }()
		if err := await(ctx, func() bool {
			state, err := client.Desktop.State(ctx, connect.NewRequest(&hostv1.DesktopServiceStateRequest{}))
			return err == nil && state.Msg.GetDesktop().GetAvailable()
		}); err != nil {
			t.Fatalf("waiting for box desktop serve: %v", err)
		}
	}
	return hostFixture{ssh: linktest.Start(t, host.RootSocket()), runtime: dir, user: current.Username, userSocket: host.UserSocket()}
}

func startXvfb(ctx context.Context, t *testing.T) string {
	t.Helper()
	program, err := exec.LookPath("Xvfb")
	if err != nil {
		if os.Getenv("CI") != "" {
			t.Fatal("Xvfb is required for the desktop test in CI")
		}
		t.Skip("Xvfb is required for the desktop test")
	}
	ready, readyWriter, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = ready.Close() }()
	defer func() { _ = readyWriter.Close() }()
	displayCtx, cancel := context.WithCancel(ctx)
	cmd := exec.CommandContext(displayCtx, program, "-displayfd", "3", "-screen", "0", "320x200x24", "-nolisten", "tcp", "-ac") //nolint:gosec // the test's Xvfb executable.
	cmd.ExtraFiles = []*os.File{readyWriter}
	if err := cmd.Start(); err != nil {
		cancel()
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cancel()
		_ = cmd.Wait()
	})
	_ = readyWriter.Close()
	display, err := bufio.NewReader(ready).ReadString('\n')
	if err != nil {
		t.Fatalf("waiting for Xvfb: %v", err)
	}
	return ":" + strings.TrimSpace(display)
}

func paintDesktop(t *testing.T, display string) *xgb.Conn {
	t.Helper()
	connection, err := xgb.NewConnDisplay(display)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(connection.Close)
	screen := xproto.Setup(connection).DefaultScreen(connection)
	window, err := xproto.NewWindowId(connection)
	if err != nil {
		t.Fatal(err)
	}
	if err := xproto.CreateWindowChecked(connection, screen.RootDepth, window, screen.Root,
		10, 10, 110, 90, 0, xproto.WindowClassInputOutput, screen.RootVisual,
		xproto.CwBackPixel, []uint32{0x3a90bc}).Check(); err != nil {
		t.Fatal(err)
	}
	if err := xproto.MapWindowChecked(connection, window).Check(); err != nil {
		t.Fatal(err)
	}
	return connection
}

func assertPointer(t *testing.T, connection *xgb.Conn, x, y int16) {
	t.Helper()
	root := xproto.Setup(connection).DefaultScreen(connection).Root
	pointer, err := xproto.QueryPointer(connection, root).Reply()
	if err != nil || pointer == nil {
		t.Fatalf("reading the pointer: %v", err)
	}
	if pointer.RootX != x || pointer.RootY != y {
		t.Fatalf("pointer after input is at (%d,%d), want (%d,%d)", pointer.RootX, pointer.RootY, x, y)
	}
}

func readRFB(conn net.Conn) error {
	defer func() { _ = conn.Close() }()
	if err := conn.SetDeadline(time.Now().Add(4 * time.Second)); err != nil {
		return err
	}
	if _, err := io.WriteString(conn, rfbVersion); err != nil {
		return err
	}
	greeting := make([]byte, len(rfbVersion))
	if _, err := io.ReadFull(conn, greeting); err != nil {
		return err
	}
	if string(greeting) != rfbVersion {
		return errors.New("the VNC viewer sent the wrong version")
	}
	_, err := io.WriteString(conn, rfbSecurity)
	return err
}
