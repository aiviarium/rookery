package agent

import (
	"context"
	"errors"
	"os"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
)

func (h *desktopService) Environment(ctx context.Context, _ *connect.Request[hostv1.DesktopServiceEnvironmentRequest]) (*connect.Response[hostv1.DesktopServiceEnvironmentResponse], error) {
	if _, err := callerOf(ctx); err != nil {
		return nil, err
	}
	env, err := readEnvFile(h.server.config.DesktopEnvPath)
	if errors.Is(err, os.ErrNotExist) {
		return connect.NewResponse(&hostv1.DesktopServiceEnvironmentResponse{}), nil
	}
	if err := h.server.authorizeDesktop(ctx); err != nil {
		return nil, err
	}
	if err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&hostv1.DesktopServiceEnvironmentResponse{Variables: env}), nil
}
