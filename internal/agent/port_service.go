package agent

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"strconv"

	"connectrpc.com/connect"
	"golang.org/x/sync/errgroup"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
)

// portService serves PortService: bytes to a port on the host's loopback, or to
// a Unix socket there.
type portService struct {
	server *Server
}

func (h *portService) Dial(ctx context.Context, stream *connect.BidiStream[hostv1.PortServiceDialRequest, hostv1.PortServiceDialResponse]) error {
	caller, err := callerOf(ctx)
	if err != nil {
		return err
	}
	if caller.role != roleRoot {
		return denied("agent: ports are for the server, not for local programs")
	}

	first, err := stream.Receive()
	if errors.Is(err, io.EOF) {
		return nil
	}
	if err != nil {
		return err
	}

	if first.GetDesktop() {
		return h.dialDesktop(ctx, stream, first)
	}

	network, address, err := targetOf(first)
	if err != nil {
		return toConnect(err)
	}

	dialer := net.Dialer{}
	conn, err := dialer.DialContext(ctx, network, address)
	if err != nil {
		return connect.NewError(connect.CodeUnavailable, fmt.Errorf("agent: dialing %s %s: %w", network, address, err))
	}
	defer func() { _ = conn.Close() }()

	group, ctx := errgroup.WithContext(ctx)
	group.Go(func() error { return writeToPort(ctx, stream, conn, first.GetData()) })
	group.Go(func() error { return readFromPort(ctx, stream, conn) })
	return group.Wait()
}

// targetOf is where a Dial's first message says to connect.
func targetOf(request *hostv1.PortServiceDialRequest) (network, address string, err error) {
	switch target := request.GetTarget().(type) {
	case *hostv1.PortServiceDialRequest_TcpPort:
		if target.TcpPort == 0 || target.TcpPort > 65535 {
			return "", "", fmt.Errorf("%w: no port %d", ErrInvalid, target.TcpPort)
		}
		return "tcp", net.JoinHostPort("127.0.0.1", strconv.FormatUint(uint64(target.TcpPort), 10)), nil
	case *hostv1.PortServiceDialRequest_UnixPath:
		if target.UnixPath == "" {
			return "", "", fmt.Errorf("%w: no Unix socket path", ErrInvalid)
		}
		return "unix", target.UnixPath, nil
	default:
		return "", "", fmt.Errorf("%w: a Dial's first message needs a port or a Unix socket path", ErrInvalid)
	}
}

// writeToPort sends the caller's bytes to the port, and closes the sending half
// when the caller is done so the other side sees the end of the stream.
func writeToPort(ctx context.Context, stream *connect.BidiStream[hostv1.PortServiceDialRequest, hostv1.PortServiceDialResponse], conn net.Conn, first []byte) error {
	if len(first) > 0 {
		if _, err := conn.Write(first); err != nil {
			return connect.NewError(connect.CodeUnavailable, fmt.Errorf("agent: writing to the port: %w", err))
		}
	}

	for {
		request, err := stream.Receive()
		if errors.Is(err, io.EOF) {
			if closeWriter, ok := conn.(interface{ CloseWrite() error }); ok {
				_ = closeWriter.CloseWrite()
			}
			return nil
		}
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			return err
		}
		if len(request.GetData()) == 0 {
			continue
		}
		if _, err := conn.Write(request.GetData()); err != nil {
			if ctx.Err() != nil {
				return ctx.Err()
			}
			return connect.NewError(connect.CodeUnavailable, fmt.Errorf("agent: writing to the port: %w", err))
		}
	}
}

// readFromPort sends the port's bytes back until it closes.
func readFromPort(ctx context.Context, stream *connect.BidiStream[hostv1.PortServiceDialRequest, hostv1.PortServiceDialResponse], conn net.Conn) error {
	buffer := make([]byte, chunkSize)
	for {
		read, err := conn.Read(buffer)
		if read > 0 {
			if err := stream.Send(&hostv1.PortServiceDialResponse{Data: append([]byte(nil), buffer[:read]...)}); err != nil {
				if ctx.Err() != nil {
					return nil
				}
				return err
			}
		}
		if err != nil {
			if errors.Is(err, io.EOF) || ctx.Err() != nil {
				return nil
			}
			return connect.NewError(connect.CodeUnavailable, fmt.Errorf("agent: reading from the port: %w", err))
		}
	}
}
