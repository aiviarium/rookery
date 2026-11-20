package server

import (
	"net/http"

	"connectrpc.com/connect"

	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/hosts"
)

func registerHostRuntime(mux *http.ServeMux, runtime *hosts.Runtime, options []connect.HandlerOption) {
	handle := func(path string, handler http.Handler) { mux.Handle(path, handler) }
	handle(rookeryv1connect.NewTerminalServiceHandler(&hosts.TerminalService{Runtime: runtime}, options...))
	handle(rookeryv1connect.NewExecServiceHandler(&hosts.ExecService{Runtime: runtime}, options...))
	handle(rookeryv1connect.NewFileServiceHandler(&hosts.FileService{Runtime: runtime}, options...))
	handle(rookeryv1connect.NewPortServiceHandler(&hosts.PortService{Runtime: runtime}, options...))
	handle(rookeryv1connect.NewChannelServiceHandler(&hosts.ChannelService{Runtime: runtime}, options...))
	handle(rookeryv1connect.NewHostCallServiceHandler(&hosts.HostCallService{Runtime: runtime}, options...))
}
