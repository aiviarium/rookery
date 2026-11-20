package agent

import (
	"context"
	"fmt"
	"net"
	"path/filepath"

	"connectrpc.com/connect"
	"golang.org/x/sync/errgroup"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/streamio"
)

// dialDesktop finds the local session's RFB listener. The desktop account owns
// the runtime directory and could replace the path with a symlink, so check the
// peer before sending or reading any bytes.
func (h *portService) dialDesktop(ctx context.Context, stream *connect.BidiStream[hostv1.PortServiceDialRequest, hostv1.PortServiceDialResponse], first *hostv1.PortServiceDialRequest) error {
	acct, err := lookupAccount(h.server.config.DesktopUser)
	if err != nil {
		return toConnect(fmt.Errorf("%w: looking up the desktop account: %w", ErrUnavailable, err))
	}
	env, err := readEnvFile(h.server.config.DesktopEnvPath)
	if err != nil {
		return toConnect(fmt.Errorf("%w: reading the desktop environment: %w", ErrUnavailable, err))
	}
	runtimeDir := env["XDG_RUNTIME_DIR"]
	if !filepath.IsAbs(runtimeDir) {
		return toConnect(fmt.Errorf("%w: the desktop's runtime directory is missing", ErrUnavailable))
	}
	dialer := net.Dialer{}
	conn, err := dialer.DialContext(ctx, "unix", filepath.Join(runtimeDir, "rookery", "vnc.sock"))
	if err != nil {
		return toConnect(fmt.Errorf("%w: dialing the desktop: %w", ErrUnavailable, err))
	}
	defer func() { _ = conn.Close() }()
	uid, _, err := peerCredentials(conn)
	if err != nil {
		return toConnect(fmt.Errorf("%w: identifying the desktop socket's peer: %w", ErrUnavailable, err))
	}
	if uid != acct.UID {
		return toConnect(fmt.Errorf("%w: the desktop socket peer is not the configured desktop account", ErrUnavailable))
	}
	if err := stream.Send(&hostv1.PortServiceDialResponse{Connected: true}); err != nil {
		return err
	}

	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	group, groupCtx := errgroup.WithContext(ctx)
	stop := context.AfterFunc(groupCtx, func() {
		_ = conn.Close()
		streamio.InterruptReads(groupCtx)
	})
	defer stop()
	group.Go(func() error { return writeToPort(groupCtx, stream, conn, first.GetData()) })
	group.Go(func() error {
		defer cancel()
		return readFromPort(groupCtx, stream, conn)
	})
	return group.Wait()
}
