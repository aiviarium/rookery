// Command service is the plugin service the supervisor's test runs. It is not
// part of the server: it serves PluginService so a test can check that the
// supervisor started it, and that a restart is a new process.
package main

import (
	"fmt"
	"os"
	"os/signal"
	"syscall"

	"context"
	"net/http"

	"connectrpc.com/connect"

	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/plugin/v1/pluginv1connect"
	"github.com/aiviarium/rookery/pkg/sdk"
)

type service struct{}

func (service) Init(context.Context, *connect.Request[pluginv1.PluginServiceInitRequest]) (*connect.Response[pluginv1.PluginServiceInitResponse], error) {
	return connect.NewResponse(&pluginv1.PluginServiceInitResponse{}), nil
}

func (service) Health(context.Context, *connect.Request[pluginv1.PluginServiceHealthRequest]) (*connect.Response[pluginv1.PluginServiceHealthResponse], error) {
	return connect.NewResponse(&pluginv1.PluginServiceHealthResponse{Healthy: true}), nil
}

func (service) TestConnection(context.Context, *connect.Request[pluginv1.PluginServiceTestConnectionRequest]) (*connect.Response[pluginv1.PluginServiceTestConnectionResponse], error) {
	return connect.NewResponse(&pluginv1.PluginServiceTestConnectionResponse{}), nil
}

func main() {
	socket := os.Getenv("ROOKERY_PLUGIN_SOCKET")
	if socket == "" {
		fmt.Fprintln(os.Stderr, "service: ROOKERY_PLUGIN_SOCKET is not set")
		os.Exit(1)
	}
	// The test reads the environment and the pid from beside the socket: the
	// service runs with an empty environment, so it cannot be told a path.
	file, err := os.OpenFile(socket+".env", os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600) //nolint:gosec // the service writes beside its own socket
	if err != nil {
		fmt.Fprintln(os.Stderr, "service:", err)
		os.Exit(1)
	}
	fmt.Fprintf(file, "pid=%d\n", os.Getpid())
	for _, name := range []string{"PATH", "HOME", "LANG", "TZ", "ROOKERY_SOCKET", "ROOKERY_PLUGIN_SOCKET", "ROOKERY_PLUGIN_TOKEN", "SECRET_FROM_SERVER"} {
		fmt.Fprintf(file, "%s=%s\n", name, os.Getenv(name))
	}
	fmt.Fprintln(file, "--")
	_ = file.Close()

	mux := http.NewServeMux()
	path, handler := pluginv1connect.NewPluginServiceHandler(service{})
	mux.Handle(path, handler)
	ctx, cancel := signalContext()
	defer cancel()
	if err := sdk.Serve(ctx, socket, mux); err != nil {
		fmt.Fprintln(os.Stderr, "service:", err)
		os.Exit(1)
	}
}

func signalContext() (context.Context, context.CancelFunc) {
	return signal.NotifyContext(context.Background(), syscall.SIGTERM, syscall.SIGINT)
}
