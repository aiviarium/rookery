package server

import (
	"context"
	"strings"

	"connectrpc.com/connect"

	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/plugin/v1/pluginv1connect"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/config"
	"github.com/aiviarium/rookery/internal/plugins"
	"github.com/aiviarium/rookery/pkg/sdk"
)

func pluginConnectionTest(supervisor *plugins.Supervisor) func(context.Context, config.ConnectionType, map[string]string) error {
	return func(ctx context.Context, typ config.ConnectionType, values map[string]string) error {
		socket, err := supervisor.Ensure(ctx, typ.Plugin)
		if err != nil {
			return err
		}
		encoded, err := cborx.Marshal(values)
		if err != nil {
			return err
		}
		client, base := sdk.DialSocket(socket)
		defer client.CloseIdleConnections()
		rpc := pluginv1connect.NewPluginServiceClient(client, base, connect.WithGRPC())
		_, typeName, _ := strings.Cut(typ.ID, "/")
		_, err = rpc.TestConnection(ctx, connect.NewRequest(&pluginv1.PluginServiceTestConnectionRequest{Type: typeName, Config: encoded}))
		return err
	}
}
