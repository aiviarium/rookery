package link

import (
	"context"
	"errors"
	"fmt"
	"io"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/access"
)

// DesktopStream holds the host link while a viewer speaks RFB. One goroutine
// may send at the same time another receives.
type DesktopStream struct {
	stream  *connect.BidiStreamForClient[hostv1.PortServiceDialRequest, hostv1.PortServiceDialResponse]
	cancel  context.CancelFunc
	release func()
}

// OpenDesktop dials the host's desktop socket over its existing daemon link.
func (m *Manager) OpenDesktop(ctx context.Context, host string) (*DesktopStream, error) {
	client, release, err := m.Use(ctx, host)
	if err != nil {
		return nil, err
	}
	ctx, cancel := context.WithCancel(ctx)
	desktop := &DesktopStream{stream: client.Ports.Dial(ctx), cancel: cancel, release: release}
	err = desktop.stream.Send(&hostv1.PortServiceDialRequest{
		Target: &hostv1.PortServiceDialRequest_Desktop{Desktop: true},
	})
	// A send can report EOF when the server rejected the request early. Read
	// its status to return the actual reason.
	if err == nil || errors.Is(err, io.EOF) {
		var ready *hostv1.PortServiceDialResponse
		ready, err = desktop.stream.Receive()
		if err == nil && !ready.GetConnected() {
			err = fmt.Errorf("%w: host %s did not acknowledge the desktop dial", access.ErrUnavailable, host)
		}
		if errors.Is(err, io.EOF) {
			err = fmt.Errorf("%w: host %s closed the desktop dial: %w", access.ErrUnavailable, host, err)
		}
	}
	if err != nil {
		desktop.Close()
		return nil, fmt.Errorf("link: opening the desktop on %s: %w", host, err)
	}
	return desktop, nil
}

// Send forwards one chunk of RFB bytes. Empty chunks carry no data.
func (s *DesktopStream) Send(data []byte) error {
	if len(data) == 0 {
		return nil
	}
	return s.stream.Send(&hostv1.PortServiceDialRequest{Data: data})
}

// Receive returns one chunk of RFB bytes.
func (s *DesktopStream) Receive() ([]byte, error) {
	message, err := s.stream.Receive()
	if err != nil {
		return nil, err
	}
	return message.GetData(), nil
}

// CloseWrite reports the viewer's end of input to the desktop.
func (s *DesktopStream) CloseWrite() error { return s.stream.CloseRequest() }

// Close ends both sides of the stream and releases the host link.
func (s *DesktopStream) Close() {
	s.cancel()
	_ = s.stream.CloseRequest()
	_ = s.stream.CloseResponse()
	s.release()
}
