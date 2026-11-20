package cli

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"strconv"
	"strings"
	"sync"

	"github.com/spf13/cobra"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
)

func portCommand() *cobra.Command {
	var local string
	command := &cobra.Command{
		Use:   "port NAME PORT [--local ADDR]",
		Short: "Forward a local address to a port or Unix socket on a host",
		Args:  cobra.ExactArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			request := &hostv1.PortServiceDialRequest{}
			switch {
			case strings.HasPrefix(args[1], "/"):
				request.Target = &hostv1.PortServiceDialRequest_UnixPath{UnixPath: args[1]}
				if local == "" {
					local = "127.0.0.1:0"
				}
			default:
				port, err := strconv.ParseUint(args[1], 10, 16)
				if err != nil || port == 0 {
					return fmt.Errorf("cli: a port must be between 1 and 65535")
				}
				request.Target = &hostv1.PortServiceDialRequest_TcpPort{TcpPort: uint32(port)}
				if local == "" {
					local = net.JoinHostPort("127.0.0.1", args[1])
				}
			}
			session, err := open(cmd)
			if err != nil {
				return err
			}
			ctx := cmd.Context()
			listener, err := (&net.ListenConfig{}).Listen(ctx, "tcp", local)
			if err != nil {
				return fmt.Errorf("cli: opening the local port: %w", err)
			}
			defer func() { _ = listener.Close() }()
			if err := session.printer.line(listener.Addr().String()); err != nil {
				return err
			}
			return forwardListener(ctx, cmd.ErrOrStderr(), listener, session, args[0], request)
		},
	}
	command.Flags().StringVar(&local, "local", "", "local listening address, defaults to 127.0.0.1:PORT")
	return command
}

func forwardListener(ctx context.Context, stderr io.Writer, listener net.Listener, session *session, host string, dial *hostv1.PortServiceDialRequest) error {
	ctx, cancel := context.WithCancel(ctx)
	var connections sync.WaitGroup
	stop := context.AfterFunc(ctx, func() { _ = listener.Close() })
	defer func() {
		cancel()
		stop()
		connections.Wait()
	}()
	var logMu sync.Mutex
	for {
		conn, err := listener.Accept()
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			return fmt.Errorf("cli: accepting a port connection: %w", err)
		}
		connections.Go(func() {
			if err := forwardConnection(ctx, session, host, dial, conn); err != nil && ctx.Err() == nil {
				logMu.Lock()
				_, _ = fmt.Fprintf(stderr, "port: %v\n", err)
				logMu.Unlock()
			}
		})
	}
}

func forwardConnection(ctx context.Context, session *session, host string, dial *hostv1.PortServiceDialRequest, conn net.Conn) error {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	defer func() { _ = conn.Close() }()
	stop := context.AfterFunc(ctx, func() { _ = conn.Close() })
	defer stop()
	stream := session.api.Ports.Connect(ctx)
	if err := stream.Send(&rookeryv1.PortServiceConnectRequest{Host: host, Dial: dial}); err != nil && !errors.Is(err, io.EOF) {
		return err
	}
	pumped := make(chan error, 1)
	go func() {
		err := sendPortBytes(conn, stream.Send)
		if errors.Is(err, io.EOF) {
			err = stream.CloseRequest()
		}
		if err != nil {
			cancel()
		}
		pumped <- err
	}()

	var receiveErr error
	for {
		message, err := stream.Receive()
		if err != nil {
			if !errors.Is(err, io.EOF) {
				receiveErr = err
			}
			break
		}
		if _, err := conn.Write(message.GetData()); err != nil {
			receiveErr = err
			break
		}
	}
	cancel()
	_ = conn.Close()
	inputErr := <-pumped
	if inputErr != nil && !errors.Is(inputErr, net.ErrClosed) && !errors.Is(inputErr, context.Canceled) {
		return errors.Join(receiveErr, inputErr)
	}
	return receiveErr
}

func sendPortBytes(input io.Reader, send func(*rookeryv1.PortServiceConnectRequest) error) error {
	buffer := make([]byte, 32<<10)
	for {
		n, err := input.Read(buffer)
		if n > 0 {
			if err := send(&rookeryv1.PortServiceConnectRequest{
				Dial: &hostv1.PortServiceDialRequest{Data: buffer[:n]},
			}); err != nil {
				return err
			}
		}
		if err != nil {
			return err
		}
	}
}
