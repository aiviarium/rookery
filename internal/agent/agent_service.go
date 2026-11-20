package agent

import (
	"context"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
)

// agentService serves AgentService: what the daemon is.
type agentService struct {
	server *Server
}

func (h *agentService) Version(ctx context.Context, _ *connect.Request[hostv1.AgentServiceVersionRequest]) (*connect.Response[hostv1.AgentServiceVersionResponse], error) {
	if _, err := callerOf(ctx); err != nil {
		return nil, err
	}
	return connect.NewResponse(h.server.version()), nil
}
