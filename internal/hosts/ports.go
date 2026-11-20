package hosts

import (
	"context"
	"errors"
	"fmt"
	"io"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
)

// PortService forwards bytes to a host's loopback or an authorised Unix socket.
type PortService struct{ *Runtime }

// Connect relays a bidirectional stream until the host port closes.
func (s *PortService) Connect(ctx context.Context, stream *connect.BidiStream[rookeryv1.PortServiceConnectRequest, rookeryv1.PortServiceConnectResponse]) error {
	first, err := stream.Receive()
	if errors.Is(err, io.EOF) {
		return nil
	}
	if err != nil {
		return err
	}
	target := first.GetDial()
	need := access.Use
	switch target.GetTarget().(type) {
	case *hostv1.PortServiceDialRequest_UnixPath:
		need = access.Manage
	case *hostv1.PortServiceDialRequest_TcpPort:
	default:
		return access.ToConnect(fmt.Errorf("%w: a port or Unix socket is required", access.ErrInvalid))
	}
	if _, err := s.hosts.authorizeHost(ctx, first.GetHost(), need, CapabilityPorts); err != nil {
		return access.ToConnect(err)
	}
	port, err := s.hosts.deps.Links.DialPort(ctx, first.GetHost(), target)
	if err != nil {
		return access.ToConnect(err)
	}
	defer func() { _ = port.Close() }()
	inputErr := make(chan error, 1)
	go func() {
		if err := forwardPortInput(stream, port); err != nil {
			inputErr <- err
			_ = port.Close()
		}
	}()

	buffer := make([]byte, 32<<10)
	for {
		n, err := port.Read(buffer)
		if n > 0 {
			if err := stream.Send(&rookeryv1.PortServiceConnectResponse{Data: buffer[:n]}); err != nil {
				return err
			}
		}
		if err != nil {
			select {
			case input := <-inputErr:
				return access.ToConnect(input)
			default:
			}
			if errors.Is(err, io.EOF) {
				return nil
			}
			return access.ToConnect(err)
		}
	}
}

type portWriter interface {
	io.Writer
	CloseWrite() error
}

func forwardPortInput(stream *connect.BidiStream[rookeryv1.PortServiceConnectRequest, rookeryv1.PortServiceConnectResponse], out portWriter) error {
	for {
		request, err := stream.Receive()
		if errors.Is(err, io.EOF) {
			return out.CloseWrite()
		}
		if err != nil {
			return err
		}
		if request.GetHost() != "" || request.GetDial().GetTarget() != nil {
			return fmt.Errorf("%w: only the first message identifies the host and port", access.ErrInvalid)
		}
		if _, err := out.Write(request.GetDial().GetData()); err != nil {
			return err
		}
	}
}
