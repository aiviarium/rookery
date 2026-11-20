package cli

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"os/signal"
	"sync"
	"syscall"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"
	"golang.org/x/sync/errgroup"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
)

func screenshotCommand() *cobra.Command {
	var output string
	var width, height uint32
	cmd := &cobra.Command{
		Use:   "screenshot NAME",
		Short: "Save a PNG screenshot of a host's desktop",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			response, err := session.api.Desktop.Screenshot(cmd.Context(), connect.NewRequest(&rookeryv1.DesktopServiceScreenshotRequest{
				Host: args[0], Width: width, Height: height,
			}))
			if err != nil {
				return fmt.Errorf("taking a screenshot of %s: %w", args[0], err)
			}
			png := response.Msg.GetPng()
			if output == "-" {
				_, err := cmd.OutOrStdout().Write(png)
				return err
			}
			if err := os.WriteFile(output, png, 0o600); err != nil {
				return fmt.Errorf("saving screenshot: %w", err)
			}
			return nil
		},
	}
	cmd.Flags().StringVarP(&output, "output", "o", "-", "PNG output path, or - for stdout")
	cmd.Flags().Uint32Var(&width, "width", 0, "scale to this width in pixels")
	cmd.Flags().Uint32Var(&height, "height", 0, "scale to this height in pixels")
	return cmd
}

func desktopCommand() *cobra.Command {
	var local string
	cmd := &cobra.Command{
		Use:   "desktop NAME",
		Short: "Listen on a local port for a VNC viewer",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			ctx, stop := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer stop()
			return listenDesktop(ctx, session.api.Desktop, args[0], local, cmd.OutOrStdout(), cmd.ErrOrStderr())
		},
	}
	cmd.Flags().StringVar(&local, "local", "127.0.0.1:0", "local loopback address for VNC")
	return cmd
}

func listenDesktop(ctx context.Context, client rookeryv1connect.DesktopServiceClient, host, address string, output, diagnostics io.Writer) error {
	resolved, err := net.ResolveTCPAddr("tcp", address)
	if err != nil {
		return fmt.Errorf("resolving VNC listen address: %w", err)
	}
	if !resolved.IP.IsLoopback() {
		return fmt.Errorf("the VNC listen address must be on loopback")
	}
	listener, err := (&net.ListenConfig{}).Listen(ctx, "tcp", resolved.String())
	if err != nil {
		return fmt.Errorf("listening for a VNC viewer: %w", err)
	}
	defer func() { _ = listener.Close() }()
	stop := context.AfterFunc(ctx, func() { _ = listener.Close() })
	defer stop()
	if _, err := fmt.Fprintln(output, listener.Addr()); err != nil {
		return err
	}

	ctx, cancel := context.WithCancel(ctx)
	var viewers sync.WaitGroup
	var diagnosticMu sync.Mutex
	defer func() {
		cancel()
		viewers.Wait()
	}()
	for {
		viewer, err := listener.Accept()
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			return fmt.Errorf("accepting a VNC viewer: %w", err)
		}
		viewers.Go(func() {
			if err := relayViewer(ctx, client, host, viewer.(*net.TCPConn)); err != nil && ctx.Err() == nil {
				diagnosticMu.Lock()
				defer diagnosticMu.Unlock()
				_, _ = fmt.Fprintf(diagnostics, "desktop %s: %v\n", host, err)
			}
		})
	}
}

func relayViewer(ctx context.Context, client rookeryv1connect.DesktopServiceClient, host string, viewer *net.TCPConn) error {
	defer func() { _ = viewer.Close() }()
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	stream := client.Connect(ctx)
	defer func() {
		_ = stream.CloseRequest()
		_ = stream.CloseResponse()
	}()
	if err := stream.Send(&rookeryv1.DesktopServiceConnectRequest{Host: host}); err != nil && !errors.Is(err, io.EOF) {
		return err
	}

	group, groupCtx := errgroup.WithContext(ctx)
	stop := context.AfterFunc(groupCtx, func() {
		cancel()
		_ = viewer.Close()
	})
	defer stop()
	group.Go(func() error {
		buffer := make([]byte, 32*1024)
		for {
			n, err := viewer.Read(buffer)
			if n > 0 {
				sendErr := stream.Send(&rookeryv1.DesktopServiceConnectRequest{Data: buffer[:n]})
				if errors.Is(sendErr, io.EOF) || groupCtx.Err() != nil {
					return nil
				}
				if sendErr != nil {
					return sendErr
				}
			}
			switch {
			case groupCtx.Err() != nil:
				return nil
			case errors.Is(err, io.EOF):
				return stream.CloseRequest()
			case err != nil:
				return err
			}
		}
	})
	group.Go(func() error {
		defer cancel()
		for {
			response, err := stream.Receive()
			if errors.Is(err, io.EOF) {
				_ = viewer.CloseWrite()
				return nil
			}
			if err != nil {
				return err
			}
			if _, err := viewer.Write(response.GetData()); err != nil {
				return err
			}
		}
	})
	return group.Wait()
}
