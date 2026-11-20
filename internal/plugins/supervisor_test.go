package plugins

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"
	"testing"
	"time"

	"connectrpc.com/connect"

	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/plugin/v1/pluginv1connect"
	"github.com/aiviarium/rookery/pkg/sdk"
)

// source is the installed plugins the test's supervisor reads.
type source struct {
	plugins []Plugin
	dir     string
}

func (s source) List(context.Context) ([]Plugin, error) { return s.plugins, nil }
func (s source) Dir(string, string) string              { return s.dir }

// TestSupervisorRunsAService starts a real service, calls its health, kills it
// and sees the supervisor start a new process, then stops it.
func TestSupervisorRunsAService(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("the environment and memory checks are Linux's")
	}
	stateDir := t.TempDir()
	dir := filepath.Join(stateDir, "plugins", "example", "abc")
	if err := os.MkdirAll(filepath.Join(dir, "bin"), 0o750); err != nil {
		t.Fatal(err)
	}

	// The service is a real process: the test binary re-run would serve the
	// tests, so the helper is built.
	helper := filepath.Join(dir, "bin", "service")
	build := exec.CommandContext(t.Context(), "go", "build", "-o", helper, "./testdata/service") //nolint:gosec // the test builds its own helper
	build.Dir = "."
	if output, err := build.CombinedOutput(); err != nil {
		t.Fatalf("building the service: %v\n%s", err, output)
	}

	manifest := fmt.Sprintf(`id: example
version: 0.1.0
name: Example
service:
  command: {%s/%s: bin/service}
  start: lazy
`, runtime.GOOS, runtime.GOARCH)
	if err := os.WriteFile(filepath.Join(dir, "plugin.yaml"), []byte(manifest), 0o600); err != nil {
		t.Fatal(err)
	}

	t.Setenv("SECRET_FROM_SERVER", "must not reach the plugin")
	plugin := Plugin{ID: "example", Name: "Example", Version: "0.1.0", SHA256: "abc", Enabled: true, Manifest: []byte(manifest)}

	supervisor := NewSupervisor(SupervisorOptions{
		Source:   source{plugins: []Plugin{plugin}, dir: dir},
		StateDir: stateDir,
		Socket:   filepath.Join(stateDir, "server.sock"),
	})

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	// A lazy service is not started with the server.
	supervisor.StartAll(ctx)
	if supervisor.Running("example") {
		t.Fatal("a lazy service started with the server")
	}

	socket, err := supervisor.Ensure(ctx, "example")
	if err != nil {
		t.Fatal(err)
	}

	client := pluginv1connect.NewPluginServiceClient(unixClient(t, socket), "http://plugin", connect.WithGRPC())
	health, err := client.Health(ctx, connect.NewRequest(&pluginv1.PluginServiceHealthRequest{}))
	if err != nil {
		t.Fatalf("calling Health: %v", err)
	}
	if !health.Msg.GetHealthy() {
		t.Fatal("the service is not healthy")
	}

	first := readRun(t, socket)
	if first["ROOKERY_PLUGIN_SOCKET"] != socket || first["ROOKERY_PLUGIN_TOKEN"] == "" {
		t.Fatalf("the service was started with %v", first)
	}
	if first["SECRET_FROM_SERVER"] != "" {
		t.Fatal("the server's environment leaked into the service")
	}
	for name := range first {
		switch name {
		case "pid", "--", "PATH", "HOME", "LANG", "TZ", "ROOKERY_SOCKET", "ROOKERY_PLUGIN_SOCKET", "ROOKERY_PLUGIN_TOKEN", "SECRET_FROM_SERVER":
		default:
			t.Fatalf("the service was started with %s", name)
		}
	}

	// An exited service is restarted, with a new process.
	firstPID := first["pid"]
	pid := 0
	if _, err := fmt.Sscanf(firstPID, "%d", &pid); err != nil {
		t.Fatal(err)
	}
	if err := syscall.Kill(pid, syscall.SIGTERM); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(20 * time.Second)
	for {
		runs := readRuns(t, socket)
		if len(runs) >= 2 && runs[len(runs)-1]["pid"] != firstPID {
			// A new process records its start before its listener is ready.
			health, err = client.Health(ctx, connect.NewRequest(&pluginv1.PluginServiceHealthRequest{}))
			if err == nil {
				break
			}
		}
		if time.Now().After(deadline) {
			t.Fatalf("the restarted service did not become healthy: %v", err)
		}
		time.Sleep(50 * time.Millisecond)
	}

	if !health.Msg.GetHealthy() {
		t.Fatal("the restarted service is not healthy")
	}

	supervisor.Stop("example")
	if supervisor.Running("example") {
		t.Fatal("the service is still running")
	}
	if _, err := os.Stat(socket); !os.IsNotExist(err) {
		t.Fatalf("the socket is still there: %v", err)
	}
}

// unixClient is an HTTP client that dials a Unix socket.
func unixClient(t *testing.T, socket string) *http.Client {
	t.Helper()
	client, err := sdk.NewClient(sdk.Config{Socket: socket})
	if err != nil {
		t.Fatal(err)
	}
	return client.HTTPClient()
}

// readRuns reads the environment the service wrote beside its socket, one block
// per process.
func readRuns(t *testing.T, socket string) []map[string]string {
	t.Helper()
	data, err := os.ReadFile(socket + ".env") //nolint:gosec // the test reads what its own service wrote
	if err != nil {
		return nil
	}
	var runs []map[string]string
	current := map[string]string{}
	for _, line := range strings.Split(string(data), "\n") {
		if line == "--" {
			runs = append(runs, current)
			current = map[string]string{}
			continue
		}
		if name, value, found := strings.Cut(line, "="); found {
			current[name] = value
		}
	}
	return runs
}

func readRun(t *testing.T, socket string) map[string]string {
	t.Helper()
	runs := readRuns(t, socket)
	if len(runs) == 0 {
		t.Fatalf("%s has no run recorded", socket)
	}
	return runs[len(runs)-1]
}
