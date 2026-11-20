package pipeconn_test

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net"
	"net/http"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/types/known/wrapperspb"

	"github.com/aiviarium/rookery/internal/pipeconn"
)

// The tests need a real gRPC call but no generated code: connect types a client
// and a handler from any two messages, so the echo service below is a bytes
// wrapper in and out.
const (
	echoProcedure = "/pipeconn.test.v1.Echo/Echo"
	echoURL       = "http://rookery.test" + echoProcedure
)

// echo serves Echo, which answers with what it was given, after handing the call
// to onCall when that is set.
func echo(t *testing.T, onCall func(ctx context.Context, in []byte) ([]byte, error)) http.Handler {
	t.Helper()

	mux := http.NewServeMux()
	mux.Handle(echoProcedure, connect.NewUnaryHandler(
		echoProcedure,
		func(ctx context.Context, req *connect.Request[wrapperspb.BytesValue]) (*connect.Response[wrapperspb.BytesValue], error) {
			in := req.Msg.GetValue()
			if onCall != nil {
				out, err := onCall(ctx, in)
				if err != nil {
					return nil, err
				}
				return connect.NewResponse(wrapperspb.Bytes(out)), nil
			}
			return connect.NewResponse(wrapperspb.Bytes(in)), nil
		},
	))
	return mux
}

// serving is a Serve call running in a goroutine, whose result can be waited for
// more than once.
type serving struct {
	done chan struct{}
	err  error
}

func start(ctx context.Context, conn net.Conn, h http.Handler) *serving {
	s := &serving{done: make(chan struct{})}
	go func() {
		defer close(s.done)
		s.err = pipeconn.Serve(ctx, conn, h)
	}()
	return s
}

func (s *serving) wait(t *testing.T) error {
	t.Helper()

	select {
	case <-s.done:
		return s.err
	case <-time.After(10 * time.Second):
		t.Fatal("the server did not return")
		return nil
	}
}

// link is a client and a server end of one connection, serving until the test
// ends.
type link struct {
	conn   net.Conn
	client *connect.Client[wrapperspb.BytesValue, wrapperspb.BytesValue]
	served *serving
}

// newLink builds a link over two io.Pipes, which, unlike net.Pipe, can be
// half-closed.
func newLink(t *testing.T, h http.Handler) *link {
	t.Helper()

	toServerReader, toServerWriter := io.Pipe()
	toClientReader, toClientWriter := io.Pipe()

	client, err := pipeconn.New(pipeconn.Pair{Reader: toClientReader, Writer: toServerWriter})
	if err != nil {
		t.Fatalf("client: %v", err)
	}
	server, err := pipeconn.New(pipeconn.Pair{Reader: toServerReader, Writer: toClientWriter})
	if err != nil {
		t.Fatalf("server: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	served := start(ctx, server, h)

	t.Cleanup(func() {
		cancel()
		_ = client.Close()
		_ = served.wait(t)
	})
	return &link{
		conn:   client,
		client: connect.NewClient[wrapperspb.BytesValue, wrapperspb.BytesValue](pipeconn.Client(client), echoURL, connect.WithGRPC()),
		served: served,
	}
}

// newNetLink builds a link over one net.Pipe, which cannot be half-closed.
func newNetLink(t *testing.T, h http.Handler) *link {
	t.Helper()

	first, second := net.Pipe()
	client, err := pipeconn.New(pipeconn.Pair{Reader: second, Writer: second})
	if err != nil {
		t.Fatalf("client: %v", err)
	}
	server, err := pipeconn.New(pipeconn.Pair{Reader: first, Writer: first})
	if err != nil {
		t.Fatalf("server: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	served := start(ctx, server, h)

	t.Cleanup(func() {
		cancel()
		_ = client.Close()
		_ = served.wait(t)
	})
	return &link{
		conn:   client,
		client: connect.NewClient[wrapperspb.BytesValue, wrapperspb.BytesValue](pipeconn.Client(client), echoURL, connect.WithGRPC()),
		served: served,
	}
}

func call(ctx context.Context, client *connect.Client[wrapperspb.BytesValue, wrapperspb.BytesValue], in []byte) ([]byte, error) {
	res, err := client.CallUnary(ctx, connect.NewRequest(wrapperspb.Bytes(in)))
	if err != nil {
		return nil, err
	}
	return res.Msg.GetValue(), nil
}

func TestUnary(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name string
		link func(t *testing.T, h http.Handler) *link
	}{
		{name: "io.Pipe", link: newLink},
		{name: "net.Pipe", link: newNetLink},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			l := tc.link(t, echo(t, nil))
			client := l.client

			// Three calls over one connection: HTTP/2 shares it between streams.
			for i, want := range [][]byte{[]byte("hello"), []byte("again"), {}} {
				got, err := call(t.Context(), client, want)
				if err != nil {
					t.Fatalf("call %d: %v", i, err)
				}
				if !bytes.Equal(got, want) {
					t.Errorf("call %d: got %q, want %q", i, got, want)
				}
			}
		})
	}
}

// TestLargeMessage carries more than the 4 MiB a gRPC message defaults to: the
// limit belongs to the message, not to the pipe.
func TestLargeMessage(t *testing.T) {
	t.Parallel()

	l := newLink(t, echo(t, nil))

	payload := bytes.Repeat([]byte("rookery"), 5<<20/7+1)
	if len(payload) < 4<<20 {
		t.Fatalf("the payload is only %d bytes", len(payload))
	}

	got, err := call(t.Context(), l.client, payload)
	if err != nil {
		t.Fatalf("call: %v", err)
	}
	if !bytes.Equal(got, payload) {
		t.Errorf("the answer differs: %d bytes in, %d bytes out", len(payload), len(got))
	}
}

// TestCancel checks that a canceled call ends the call and nothing else: the
// handler sees its context end, and the connection still carries the next call.
func TestCancel(t *testing.T) {
	t.Parallel()

	started := make(chan struct{})
	release := make(chan struct{})
	var sawCancel atomic.Bool
	var once sync.Once

	l := newLink(t, echo(t, func(ctx context.Context, in []byte) ([]byte, error) {
		once.Do(func() { close(started) })
		select {
		case <-ctx.Done():
			sawCancel.Store(true)
		case <-release:
		}
		return in, nil
	}))

	ctx, cancel := context.WithCancel(t.Context())
	called := make(chan error, 1)
	go func() {
		_, err := call(ctx, l.client, []byte("slow"))
		called <- err
	}()

	select {
	case <-started:
	case <-time.After(10 * time.Second):
		t.Fatal("the handler never started")
	}
	cancel()

	select {
	case err := <-called:
		if err == nil {
			t.Fatal("the call succeeded after its context was canceled")
		}
		if !errors.Is(err, context.Canceled) {
			t.Errorf("got %v, want it to be context.Canceled", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("the call did not end after its context was canceled")
	}

	// The handler's context ends with the stream, not with the connection.
	if err := waitFor(sawCancel.Load); err != nil {
		t.Error(err)
	}
	// Unblock a handler that missed the cancellation, so the test can end.
	close(release)

	got, err := call(t.Context(), l.client, []byte("after"))
	if err != nil {
		t.Fatalf("the call after a cancellation: %v", err)
	}
	if string(got) != "after" {
		t.Errorf("got %q, want %q", got, "after")
	}
}

// TestHalfClose checks that closing the writing half ends the session on the
// server while reading stays open, and that no call follows it.
func TestHalfClose(t *testing.T) {
	t.Parallel()

	l := newLink(t, echo(t, nil))

	got, err := call(t.Context(), l.client, []byte("before"))
	if err != nil {
		t.Fatalf("call: %v", err)
	}
	if string(got) != "before" {
		t.Errorf("got %q, want %q", got, "before")
	}

	if err := l.conn.(interface{ CloseWrite() error }).CloseWrite(); err != nil {
		t.Fatalf("CloseWrite: %v", err)
	}

	if err := l.served.wait(t); err != nil {
		t.Errorf("Serve: %v", err)
	}

	if _, err := call(t.Context(), l.client, []byte("after")); err == nil {
		t.Fatal("a call after the half-close succeeded")
	}
}

// TestServeContext checks that Serve returns the context's error and closes the
// connection when it is canceled.
func TestServeContext(t *testing.T) {
	t.Parallel()

	blocked, blockedWriter := io.Pipe()
	t.Cleanup(func() { _ = blockedWriter.Close() })

	conn, err := pipeconn.New(pipeconn.Pair{Reader: blocked, Writer: nopCloser{Writer: io.Discard}})
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	served := start(ctx, conn, echo(t, nil))
	cancel()

	if err := served.wait(t); !errors.Is(err, context.Canceled) {
		t.Errorf("got %v, want context.Canceled", err)
	}
}

// TestConn covers the net.Conn contract itself: a write on one end is a read on
// the other, and both ends end with a close.
func TestConn(t *testing.T) {
	t.Parallel()

	rightReader, leftWriter := io.Pipe()
	leftReader, rightWriter := io.Pipe()

	left, err := pipeconn.New(pipeconn.Pair{Reader: leftReader, Writer: leftWriter})
	if err != nil {
		t.Fatalf("left: %v", err)
	}
	right, err := pipeconn.New(pipeconn.Pair{Reader: rightReader, Writer: rightWriter})
	if err != nil {
		t.Fatalf("right: %v", err)
	}
	t.Cleanup(func() {
		_ = left.Close()
		_ = right.Close()
	})

	buf := make([]byte, 5)
	read := make(chan error, 1)
	go func() {
		_, err := io.ReadFull(right, buf)
		read <- err
	}()
	if _, err := left.Write([]byte("hello")); err != nil {
		t.Fatalf("Write: %v", err)
	}
	if err := <-read; err != nil {
		t.Fatalf("Read: %v", err)
	}
	if string(buf) != "hello" {
		t.Errorf("got %q, want %q", buf, "hello")
	}

	if err := left.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if _, err := left.Write([]byte("x")); !errors.Is(err, net.ErrClosed) {
		t.Errorf("Write after Close: got %v, want net.ErrClosed", err)
	}
	if _, err := right.Read(buf); !errors.Is(err, io.EOF) {
		t.Errorf("Read after the peer's Close: got %v, want io.EOF", err)
	}

	if err := left.SetDeadline(time.Now()); err != nil {
		t.Errorf("SetDeadline: %v", err)
	}
	if got, want := left.LocalAddr().Network(), "pipe"; got != want {
		t.Errorf("Network: got %q, want %q", got, want)
	}
}

// TestNewRefusesHalfAPair checks the constructor's one requirement.
func TestNewRefusesHalfAPair(t *testing.T) {
	t.Parallel()

	if _, err := pipeconn.New(pipeconn.Pair{Writer: nopCloser{Writer: io.Discard}}); err == nil {
		t.Error("New accepted a Pair without a reader")
	}
	if _, err := pipeconn.New(pipeconn.Pair{Reader: nopCloser{Reader: bytes.NewReader(nil)}}); err == nil {
		t.Error("New accepted a Pair without a writer")
	}
}

func waitFor(condition func() bool) error {
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		if condition() {
			return nil
		}
		time.Sleep(time.Millisecond)
	}
	return errors.New("the condition did not hold in time")
}

// nopCloser gives a ReadCloser or a WriteCloser around a plain reader or writer.
type nopCloser struct {
	io.Reader
	io.Writer
}

func (nopCloser) Close() error { return nil }
