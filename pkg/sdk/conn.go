package sdk

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"sync"
	"time"

	"connectrpc.com/connect"

	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
)

// ErrStreamClosed is a read or write on a connection that is closed.
var ErrStreamClosed = errors.New("sdk: the stream is closed")

// nodeConn is a net.Conn over a DialNode stream.
//
// It has no deadlines of its own: the stream ends with the caller's context, and
// gRPC has no deadline on a stream to set. SetDeadline and its siblings are
// accepted and ignored; put a deadline on the context instead.
type nodeConn struct {
	target Target
	stream *connect.BidiStreamForClient[pluginv1.ServerServiceDialNodeRequest, pluginv1.ServerServiceDialNodeResponse]

	// reader carries what the node sent, and is closed with the stream's error.
	reader *io.PipeReader
	writer *io.PipeWriter

	// cancel ends the stream.
	cancel context.CancelFunc

	// write serialises writes and refuses them after a half-close.
	writeMu   sync.Mutex
	writeDone bool

	closeOnce sync.Once
}

func newNodeConn(target Target, stream *connect.BidiStreamForClient[pluginv1.ServerServiceDialNodeRequest, pluginv1.ServerServiceDialNodeResponse], cancel context.CancelFunc) *nodeConn {
	reader, writer := io.Pipe()
	return &nodeConn{
		target: target,
		stream: stream,
		reader: reader,
		writer: writer,
		cancel: cancel,
	}
}

// receive reads the stream into the reader until it ends.
func (c *nodeConn) receive() {
	for {
		message, err := c.stream.Receive()
		if err != nil {
			if errors.Is(err, io.EOF) {
				err = io.EOF
			}
			_ = c.writer.CloseWithError(err)
			return
		}
		if data := message.GetData(); len(data) > 0 {
			if _, err := c.writer.Write(data); err != nil {
				return
			}
		}
	}
}

// Read reads what the node sent.
func (c *nodeConn) Read(p []byte) (int, error) { return c.reader.Read(p) }

// Write sends p to the node.
func (c *nodeConn) Write(p []byte) (int, error) {
	c.writeMu.Lock()
	defer c.writeMu.Unlock()
	if c.writeDone {
		return 0, ErrStreamClosed
	}
	data := make([]byte, len(p))
	copy(data, p)
	if err := c.stream.Send(&pluginv1.ServerServiceDialNodeRequest{Data: data}); err != nil {
		return 0, fmt.Errorf("sdk: writing to %s: %w", c.target, err)
	}
	return len(p), nil
}

// CloseWrite half-closes the stream towards the node, which sees the end of what
// the caller sends and may still answer.
func (c *nodeConn) CloseWrite() error {
	c.writeMu.Lock()
	defer c.writeMu.Unlock()
	c.writeDone = true
	if err := c.stream.CloseRequest(); err != nil {
		return fmt.Errorf("sdk: half-closing %s: %w", c.target, err)
	}
	return nil
}

// Close ends the stream.
func (c *nodeConn) Close() error {
	c.closeOnce.Do(func() {
		c.cancel()
		// A Read waiting on a stream the other side does not end must return.
		_ = c.writer.CloseWithError(ErrStreamClosed)
	})
	return nil
}

// LocalAddr is where the connection starts, which is this process.
func (c *nodeConn) LocalAddr() net.Addr { return nodeAddr{endpoint: c.target} }

// RemoteAddr is where the connection goes.
func (c *nodeConn) RemoteAddr() net.Addr { return nodeAddr{endpoint: c.target} }

// SetDeadline is accepted and does not end the call: gRPC carries its own.
func (c *nodeConn) SetDeadline(time.Time) error { return nil }

// SetReadDeadline is accepted and does not end the call.
func (c *nodeConn) SetReadDeadline(time.Time) error { return nil }

// SetWriteDeadline is accepted and does not end the call.
func (c *nodeConn) SetWriteDeadline(time.Time) error { return nil }

// nodeAddr is one end of a node connection, as net.Conn's addresses are.
type nodeAddr struct{ endpoint Target }

func (a nodeAddr) Network() string {
	if a.endpoint.UnixPath != "" {
		return "unix"
	}
	return "tcp"
}

func (a nodeAddr) String() string { return a.endpoint.String() }
