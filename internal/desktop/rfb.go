package desktop

import (
	"context"
	"errors"
	"fmt"
	"io"
	"time"

	"connectrpc.com/connect"
	"golang.org/x/sync/errgroup"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/link"
	"github.com/aiviarium/rookery/internal/streamio"
)

func (s *Service) authorizeViewer(ctx context.Context, host string) error {
	if _, err := access.ParseResource(access.HostURI(host)); err != nil {
		return err
	}
	if err := s.checker.Check(ctx, access.MustPrincipal(ctx), access.HostURI(host), access.Use); err != nil {
		return err
	}
	return s.checkHost(ctx, host)
}

func (s *Service) openDesktop(ctx context.Context, cancel context.CancelFunc, host string) (*link.DesktopStream, error) {
	deadline := time.AfterFunc(15*time.Second, cancel)
	defer deadline.Stop()
	return s.links.OpenDesktop(ctx, host)
}

// Connect relays RFB to a VNC viewer using the CLI's gRPC connection.
func (s *Service) Connect(ctx context.Context, stream *connect.BidiStream[rookeryv1.DesktopServiceConnectRequest, rookeryv1.DesktopServiceConnectResponse]) error {
	first, err := stream.Receive()
	if err != nil {
		return err
	}
	if err := s.authorizeViewer(ctx, first.GetHost()); err != nil {
		return access.ToConnect(err)
	}
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	desktop, err := s.openDesktop(ctx, cancel, first.GetHost())
	if err != nil {
		return access.ToConnect(err)
	}
	defer desktop.Close()

	group, groupCtx := errgroup.WithContext(ctx)
	stop := context.AfterFunc(groupCtx, func() {
		cancel()
		streamio.InterruptReads(groupCtx)
	})
	defer stop()
	group.Go(func() error {
		return forwardToDesktop(groupCtx, stream, desktop, first.GetData())
	})
	group.Go(func() error {
		defer cancel()
		for {
			data, err := desktop.Receive()
			if errors.Is(err, io.EOF) {
				return nil
			}
			if err != nil {
				return err
			}
			if err := stream.Send(&rookeryv1.DesktopServiceConnectResponse{Data: data}); err != nil {
				return err
			}
		}
	})
	return access.ToConnect(group.Wait())
}

func forwardToDesktop(ctx context.Context, stream *connect.BidiStream[rookeryv1.DesktopServiceConnectRequest, rookeryv1.DesktopServiceConnectResponse], desktop *link.DesktopStream, first []byte) error {
	if err := desktop.Send(first); err != nil {
		if errors.Is(err, io.EOF) {
			return nil // The response carries the upstream status.
		}
		return err
	}
	for {
		request, err := stream.Receive()
		switch {
		case ctx.Err() != nil:
			return nil
		case errors.Is(err, io.EOF):
			return desktop.CloseWrite()
		case err != nil:
			return err
		case request.GetHost() != "":
			return fmt.Errorf("%w: only the opening message names the desktop host", access.ErrInvalid)
		}
		if err := desktop.Send(request.GetData()); err != nil {
			if errors.Is(err, io.EOF) || ctx.Err() != nil {
				return nil
			}
			return err
		}
	}
}
