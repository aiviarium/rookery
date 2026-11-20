// Package pipeconn carries HTTP/2, and with it gRPC, over byte streams that are
// not sockets: an SSH session's stdio, for instance, where the server runs
// `sudo -n rookeryd connect` and the session's stdout and stdin become the two
// halves of a connection.
//
// A Pair is adapted to a net.Conn, Serve serves an http.Handler on one with
// HTTP/2 in cleartext, and Client gives the other end an http.Client that
// connect-go uses with connect.WithGRPC().
//
// The connection speaks HTTP/2 with prior knowledge: no TLS, no HTTP/1, no
// upgrade. net/http does that through Protocols with UnencryptedHTTP2 alone,
// which is what x/net/http2's ServeConn and Transport used to do before they
// were deprecated.
package pipeconn

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"sync"
	"sync/atomic"
	"time"
)

// Pair is the two byte streams a connection is made of: the one it reads from
// and the one it writes to.
type Pair struct {
	Reader io.ReadCloser
	Writer io.WriteCloser
}

// ErrDialed is returned when a request wants a new connection and the one the
// client was made for is gone. There is no other path to the peer.
var ErrDialed = errors.New("pipeconn: the connection was already dialed")

// New adapts a Pair to a net.Conn.
//
// Closing the connection closes both halves. CloseWrite closes only the writing
// half, so the peer reads the end of the stream while reading stays open, which
// is how a session ends a request without dropping the answer. There are no
// deadlines to set on a pipe; they are accepted and ignored.
func New(pair Pair) (net.Conn, error) {
	if pair.Reader == nil || pair.Writer == nil {
		return nil, errors.New("pipeconn: a Pair needs a reader and a writer")
	}
	return &conn{reader: pair.Reader, writer: pair.Writer}, nil
}

// Serve serves h over conn with HTTP/2 in cleartext until the connection ends or
// ctx is canceled. It closes conn and waits for the streams to finish when ctx is
// canceled, and then returns ctx.Err().
func Serve(ctx context.Context, conn net.Conn, h http.Handler) error {
	if h == nil {
		return errors.New("pipeconn: Serve needs a handler")
	}

	// The connection ends when the peer closes it: the server keeps serving, as
	// servers do, so its state is what says the session is over.
	ended := make(chan struct{})
	var endedOnce sync.Once

	server := &http.Server{
		Handler:   h,
		Protocols: cleartextHTTP2(),
		// A server is asked for a header timeout; a pipe's deadlines are
		// ignored, so this never fires. The link decides when a connection is
		// idle, not the transport.
		ReadHeaderTimeout: 30 * time.Second,
		ConnState: func(_ net.Conn, state http.ConnState) {
			if state == http.StateClosed || state == http.StateHijacked {
				endedOnce.Do(func() { close(ended) })
			}
		},
	}

	listener := &oneConnListener{conn: conn, closed: make(chan struct{})}
	served := make(chan error, 1)
	go func() { served <- server.Serve(listener) }()

	select {
	case <-ctx.Done():
		_ = server.Close()
		if err := <-served; err != nil && !errors.Is(err, http.ErrServerClosed) {
			return fmt.Errorf("pipeconn: serving: %w", err)
		}
		return ctx.Err()
	case <-ended:
		// Closing the listener is what ends Serve; the connection is done
		// already.
		_ = server.Close()
		if err := <-served; err != nil && !errors.Is(err, http.ErrServerClosed) {
			return fmt.Errorf("pipeconn: serving: %w", err)
		}
		return nil
	}
}

// Client returns an HTTP client that speaks HTTP/2 in cleartext over conn. Its
// requests dial conn; when that connection is gone, a request fails with
// ErrDialed rather than opening another.
//
// A connect client is made from it with connect.WithGRPC():
//
//	pipeconn.Client(conn) // then connect.NewClient(…, connect.WithGRPC())
func Client(conn net.Conn) *http.Client {
	var (
		mu     sync.Mutex
		dialed bool
	)

	transport := &http.Transport{
		Protocols:       cleartextHTTP2(),
		MaxConnsPerHost: 1,
		// A pipe is already addressed: the address in the URL names nothing.
		DialContext: func(_ context.Context, _, _ string) (net.Conn, error) {
			mu.Lock()
			defer mu.Unlock()
			if dialed {
				return nil, ErrDialed
			}
			dialed = true
			return conn, nil
		},
		// gRPC frames its own messages; the transport must not decode them.
		DisableCompression: true,
	}
	return &http.Client{Transport: transport}
}

// cleartextHTTP2 is HTTP/2 without TLS and without HTTP/1.
func cleartextHTTP2() *http.Protocols {
	protocols := new(http.Protocols)
	protocols.SetUnencryptedHTTP2(true)
	return protocols
}

// oneConnListener hands a server the one connection there is, and then blocks
// until the server closes the listener.
type oneConnListener struct {
	conn     net.Conn
	accepted atomic.Bool
	closed   chan struct{}
	closeOne sync.Once
}

func (l *oneConnListener) Accept() (net.Conn, error) {
	if l.accepted.CompareAndSwap(false, true) {
		return l.conn, nil
	}
	<-l.closed
	return nil, net.ErrClosed
}

func (l *oneConnListener) Close() error {
	l.closeOne.Do(func() { close(l.closed) })
	return nil
}

func (l *oneConnListener) Addr() net.Addr { return l.conn.LocalAddr() }

// conn is a net.Conn over a Pair.
type conn struct {
	reader io.ReadCloser
	writer io.WriteCloser

	readMu  sync.Mutex
	writeMu sync.Mutex

	closed      atomic.Bool
	writeClosed atomic.Bool
	closeOnce   sync.Once
	closeErr    error
}

var _ net.Conn = (*conn)(nil)

func (c *conn) Read(p []byte) (int, error) {
	if c.closed.Load() {
		return 0, net.ErrClosed
	}

	c.readMu.Lock()
	defer c.readMu.Unlock()

	n, err := c.reader.Read(p)
	if errors.Is(err, io.ErrClosedPipe) {
		// The reader was closed under a waiting Read: that is a closed
		// connection, not a broken pipe.
		return n, net.ErrClosed
	}
	if err != nil {
		// io.EOF included: the peer wrote its end.
		return n, err
	}
	return n, nil
}

func (c *conn) Write(p []byte) (int, error) {
	if c.closed.Load() || c.writeClosed.Load() {
		return 0, net.ErrClosed
	}

	c.writeMu.Lock()
	defer c.writeMu.Unlock()

	n, err := c.writer.Write(p)
	if errors.Is(err, io.ErrClosedPipe) {
		return n, net.ErrClosed
	}
	if err != nil {
		return n, err
	}
	return n, nil
}

// CloseWrite closes the writing half. The peer reads the end of the stream, and
// reading stays open for the answer.
func (c *conn) CloseWrite() error {
	if c.writeClosed.Swap(true) {
		return nil
	}
	if err := c.writer.Close(); err != nil && !errors.Is(err, io.ErrClosedPipe) {
		return err
	}
	return nil
}

func (c *conn) Close() error {
	c.closeOnce.Do(func() {
		c.closed.Store(true)
		// Tell the peer first that nothing more comes, then stop reading.
		c.closeErr = errors.Join(c.CloseWrite(), ignorePipeClosed(c.reader.Close()))
	})
	return c.closeErr
}

func ignorePipeClosed(err error) error {
	if errors.Is(err, io.ErrClosedPipe) {
		return nil
	}
	return err
}

func (c *conn) LocalAddr() net.Addr  { return pipeAddr("pipe") }
func (c *conn) RemoteAddr() net.Addr { return pipeAddr("pipe") }

// SetDeadline, SetReadDeadline and SetWriteDeadline do nothing: a pipe has no
// timeout to set. HTTP/2 ends an idle connection with its own timer instead.
func (c *conn) SetDeadline(_ time.Time) error      { return nil }
func (c *conn) SetReadDeadline(_ time.Time) error  { return nil }
func (c *conn) SetWriteDeadline(_ time.Time) error { return nil }

// pipeAddr is the address of both ends of a pipe.
type pipeAddr string

func (a pipeAddr) Network() string { return "pipe" }
func (a pipeAddr) String() string  { return string(a) }
