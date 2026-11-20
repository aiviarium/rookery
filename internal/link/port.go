package link

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"sync"
	"time"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/access"
)

// DialPort holds the host link for the life of the TCP or Unix connection.
func (m *Manager) DialPort(ctx context.Context, host string, target *hostv1.PortServiceDialRequest) (*PortConn, error) {
	client, release, err := m.Use(ctx, host)
	if err != nil {
		return nil, err
	}
	ctx, cancel := context.WithCancel(ctx)
	stream := client.Ports.Dial(ctx)
	conn := &PortConn{stream: stream, cancel: cancel, release: release}
	if err := stream.Send(target); err != nil && !errors.Is(err, io.EOF) {
		_ = conn.Close()
		return nil, err
	}
	first, err := stream.Receive()
	if err != nil {
		_ = conn.Close()
		return nil, err
	}
	if !first.GetConnected() {
		_ = conn.Close()
		return nil, fmt.Errorf("%w: daemon did not acknowledge the port connection", access.ErrUnavailable)
	}
	conn.pending = first.GetData()
	return conn, nil
}

// PortConn presents one daemon port stream as a connection. The stream's
// context bounds its lifetime. Canceling it interrupts both reads and writes.
type PortConn struct {
	stream  *connect.BidiStreamForClient[hostv1.PortServiceDialRequest, hostv1.PortServiceDialResponse]
	cancel  context.CancelFunc
	release func()
	close   sync.Once

	readMu  sync.Mutex
	pending []byte
	writeMu sync.Mutex
}

// Read returns bytes received from the host port.
func (c *PortConn) Read(p []byte) (int, error) {
	if len(p) == 0 {
		return 0, nil
	}
	c.readMu.Lock()
	defer c.readMu.Unlock()
	for len(c.pending) == 0 {
		response, err := c.stream.Receive()
		if err != nil {
			return 0, err
		}
		c.pending = response.GetData()
	}
	n := copy(p, c.pending)
	c.pending = c.pending[n:]
	return n, nil
}

// Write sends bytes to the host port.
func (c *PortConn) Write(p []byte) (int, error) {
	c.writeMu.Lock()
	defer c.writeMu.Unlock()
	written := 0
	for len(p) > 0 {
		n := min(len(p), 32<<10)
		if err := c.stream.Send(&hostv1.PortServiceDialRequest{Data: p[:n]}); err != nil {
			return written, err
		}
		written += n
		p = p[n:]
	}
	return written, nil
}

// CloseWrite sends EOF to the host port while keeping reads open.
func (c *PortConn) CloseWrite() error {
	c.writeMu.Lock()
	defer c.writeMu.Unlock()
	return c.stream.CloseRequest()
}

// Close cancels the stream and releases its host link.
func (c *PortConn) Close() error {
	c.close.Do(func() {
		c.cancel()
		c.release()
	})
	return nil
}

// LocalAddr represents the forwarding end of the stream.
func (c *PortConn) LocalAddr() net.Addr { return &net.TCPAddr{} }

// RemoteAddr represents the loopback end of the stream.
func (c *PortConn) RemoteAddr() net.Addr { return &net.TCPAddr{IP: net.IPv4(127, 0, 0, 1)} }

// SetDeadline reports that the stream uses context cancellation.
func (c *PortConn) SetDeadline(time.Time) error { return errors.ErrUnsupported }

// SetReadDeadline reports that the stream uses context cancellation.
func (c *PortConn) SetReadDeadline(time.Time) error { return errors.ErrUnsupported }

// SetWriteDeadline reports that the stream uses context cancellation.
func (c *PortConn) SetWriteDeadline(time.Time) error { return errors.ErrUnsupported }
