package sdk

import (
	"context"
	"net/http"

	"connectrpc.com/connect"

	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/plugin/v1/pluginv1connect"
)

// Service is the lifecycle of a plugin service. Override the callbacks that
// need application logic.
type Service struct {
	OnInit           func(context.Context, *pluginv1.PluginServiceInitRequest) error
	OnHealth         func(context.Context) (bool, string)
	OnTestConnection func(context.Context, string, []byte) (string, error)
}

// Init gives the plugin its identity and settings.
func (s Service) Init(ctx context.Context, request *connect.Request[pluginv1.PluginServiceInitRequest]) (*connect.Response[pluginv1.PluginServiceInitResponse], error) {
	if s.OnInit != nil {
		if err := s.OnInit(ctx, request.Msg); err != nil {
			return nil, err
		}
	}
	return connect.NewResponse(&pluginv1.PluginServiceInitResponse{}), nil
}

// Health checks whether the service is ready.
func (s Service) Health(ctx context.Context, _ *connect.Request[pluginv1.PluginServiceHealthRequest]) (*connect.Response[pluginv1.PluginServiceHealthResponse], error) {
	healthy, message := true, ""
	if s.OnHealth != nil {
		healthy, message = s.OnHealth(ctx)
	}
	return connect.NewResponse(&pluginv1.PluginServiceHealthResponse{Healthy: healthy, Message: message}), nil
}

// TestConnection runs the plugin's test for a configured connection type.
func (s Service) TestConnection(ctx context.Context, request *connect.Request[pluginv1.PluginServiceTestConnectionRequest]) (*connect.Response[pluginv1.PluginServiceTestConnectionResponse], error) {
	if s.OnTestConnection == nil {
		return (pluginv1connect.UnimplementedPluginServiceHandler{}).TestConnection(ctx, request)
	}
	message, err := s.OnTestConnection(ctx, request.Msg.GetType(), request.Msg.GetConfig())
	if err != nil {
		return nil, err
	}
	return connect.NewResponse(&pluginv1.PluginServiceTestConnectionResponse{Message: message}), nil
}

// ServePlugin serves lifecycle and application RPCs on the service socket.
func ServePlugin(ctx context.Context, socket string, service Service, register func(*http.ServeMux)) error {
	mux := http.NewServeMux()
	path, handler := pluginv1connect.NewPluginServiceHandler(service)
	mux.Handle(path, handler)
	if register != nil {
		register(mux)
	}
	return Serve(ctx, socket, mux)
}
