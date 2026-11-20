package sdk

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"connectrpc.com/connect"

	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/plugin/v1/pluginv1connect"
	"github.com/aiviarium/rookery/internal/id"
)

// Harness is a test's stand-in for the server: a Unix socket serving
// ServerService with a token, so that a provisioner executable can be started
// the way the server starts it and dialled the way the server dials a node.
//
// It is in the SDK, and not only in a test, because a plugin's own tests need it,
// and so does the core's test of what it does with a provisioner.
type Harness struct {
	t      testing.TB
	dir    string
	config Config
	client *Client

	mu            sync.Mutex
	dials         []Target
	events        []PublishedEvent
	notifications []SentNotification
	// DialNode is how a target is reached on this machine. A test points it at
	// what it wants a node's socket to be; by default it dials the target
	// itself.
	DialNode func(ctx context.Context, target Target) (net.Conn, error)
}

// PublishedEvent is an event a plugin's service published.
type PublishedEvent struct {
	Name     string
	Resource string
	Payload  []byte
}

// SentNotification is a notification a plugin's service sent.
type SentNotification struct {
	UserID   string
	Title    string
	Body     string
	Link     string
	Resource string
}

// StartHarness starts a server socket for a test. It is closed when the test
// ends.
func StartHarness(t testing.TB) *Harness {
	t.Helper()

	dir := t.TempDir()
	socket := filepath.Join(dir, "server.sock")
	harness := &Harness{
		t:   t,
		dir: dir,
		config: Config{
			Socket:       socket,
			PluginSocket: filepath.Join(dir, "plugin.sock"),
			Token:        id.Secret(16),
		},
	}

	mux := http.NewServeMux()
	path, handler := pluginv1connect.NewServerServiceHandler(harnessServer{harness: harness})
	mux.Handle(path, handler)
	authorized := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get(TokenHeader) != "Bearer "+harness.config.Token {
			http.Error(w, "sdk: the plugin's token is not the one the harness made", http.StatusUnauthorized)
			return
		}
		mux.ServeHTTP(w, r)
	})

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		defer close(done)
		if err := Serve(ctx, socket, authorized); err != nil {
			t.Logf("sdk: the harness's server socket ended: %v", err)
		}
	}()
	t.Cleanup(func() {
		cancel()
		<-done
	})

	// Wait for the socket to answer, so the first call does not race the listen.
	deadline := time.Now().Add(5 * time.Second)
	for {
		var dialer net.Dialer
		conn, err := dialer.DialContext(ctx, "unix", socket)
		if err == nil {
			_ = conn.Close()
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("sdk: the harness's socket did not come up: %v", err)
		}
		time.Sleep(5 * time.Millisecond)
	}

	client, err := NewClient(harness.config)
	if err != nil {
		t.Fatalf("sdk: connecting to the harness's socket: %v", err)
	}
	harness.client = client
	return harness
}

// Config is what to start a plugin with: the server's socket, the socket the
// plugin serves on, and the token.
func (h *Harness) Config() Config { return h.config }

// Token is the credential the harness accepts.
func (h *Harness) Token() string { return h.config.Token }

// Client is a plugin's client for the harness's socket.
func (h *Harness) Client() *Client { return h.client }

// Dials are the targets the harness was asked to reach, in order.
func (h *Harness) Dials() []Target {
	h.mu.Lock()
	defer h.mu.Unlock()
	return append([]Target(nil), h.dials...)
}

// Run starts a provisioner executable the way the server does: with an empty
// environment but PATH, HOME, LANG and TZ, and the three Rookery variables, and
// stops it when the test ends. Its stderr is the test's log.
//
// It returns a client for the socket the program serves on. The program is
// expected to be one written on this SDK, which serves there as soon as it
// starts.
func (h *Harness) Run(command string, args ...string) *ProvisionerClient {
	h.t.Helper()

	socket := filepath.Join(h.dir, "provisioner-"+id.Secret(4)+".sock")
	config := h.config
	config.PluginSocket = socket

	ctx, cancel := context.WithCancel(context.Background())
	//nolint:gosec // the test names the executable it built.
	cmd := exec.CommandContext(ctx, command, args...)
	cmd.Env = []string{
		"PATH=" + os.Getenv("PATH"),
		"HOME=" + os.Getenv("HOME"),
		"LANG=C.UTF-8",
		"TZ=UTC",
		SocketEnv + "=" + config.Socket,
		PluginSocketEnv + "=" + config.PluginSocket,
		TokenEnv + "=" + config.Token,
	}
	stderr := &lineWriter{sink: func(line string) { h.t.Logf("provisioner: %s", line) }}
	cmd.Stderr = stderr
	cmd.Stdout = stderr
	if err := cmd.Start(); err != nil {
		cancel()
		h.t.Fatalf("sdk: starting %s: %v", command, err)
	}
	h.t.Cleanup(func() {
		cancel()
		_ = cmd.Wait()
	})

	httpClient, base := socketClient(socket)
	client := NewProvisionerClient(httpClient, base)

	// Wait until the program answers: a provisioner opens its socket as soon as
	// it starts, and the server does the same before its first call.
	deadline := time.Now().Add(15 * time.Second)
	for {
		callCtx, cancelCall := context.WithTimeout(ctx, time.Second)
		_, err := client.Describe(callCtx)
		cancelCall()
		if err == nil {
			return client
		}
		if ctx.Err() != nil {
			h.t.Fatalf("sdk: %s ended before it answered: %v", command, err)
		}
		if time.Now().After(deadline) {
			h.t.Fatalf("sdk: %s did not answer on %s: %v", command, socket, err)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// ServeProvisioner serves a provisioner in the test's own process, and returns a
// client for it. It is for testing what the core does with a provisioner without
// building one.
func (h *Harness) ServeProvisioner(provisioner Provisioner) *ProvisionerClient {
	h.t.Helper()

	socket := filepath.Join(h.dir, "inprocess-"+id.Secret(4)+".sock")
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		defer close(done)
		if err := ServeProvisionerOn(ctx, socket, provisioner); err != nil {
			h.t.Logf("sdk: the provisioner ended: %v", err)
		}
	}()
	h.t.Cleanup(func() {
		cancel()
		<-done
	})

	deadline := time.Now().Add(5 * time.Second)
	for {
		if _, err := os.Stat(socket); err == nil {
			break
		}
		if time.Now().After(deadline) {
			h.t.Fatal("sdk: the provisioner's socket did not come up")
		}
		time.Sleep(5 * time.Millisecond)
	}
	httpClient, base := socketClient(socket)
	return NewProvisionerClient(httpClient, base)
}

// harnessServer serves ServerService for a test.
type harnessServer struct{ harness *Harness }

// Publish records the event.
func (s harnessServer) Publish(_ context.Context, request *connect.Request[pluginv1.ServerServicePublishRequest]) (*connect.Response[pluginv1.ServerServicePublishResponse], error) {
	s.harness.mu.Lock()
	defer s.harness.mu.Unlock()
	s.harness.events = append(s.harness.events, PublishedEvent{
		Name:     request.Msg.GetName(),
		Resource: request.Msg.GetResource(),
		Payload:  request.Msg.GetPayloadCbor(),
	})
	return connect.NewResponse(&pluginv1.ServerServicePublishResponse{}), nil
}

// SendNotification records the notification.
func (s harnessServer) SendNotification(_ context.Context, request *connect.Request[pluginv1.ServerServiceSendNotificationRequest]) (*connect.Response[pluginv1.ServerServiceSendNotificationResponse], error) {
	s.harness.mu.Lock()
	defer s.harness.mu.Unlock()
	s.harness.notifications = append(s.harness.notifications, SentNotification{
		UserID:   request.Msg.GetUserId(),
		Title:    request.Msg.GetTitle(),
		Body:     request.Msg.GetBody(),
		Link:     request.Msg.GetLink(),
		Resource: request.Msg.GetResource(),
	})
	return connect.NewResponse(&pluginv1.ServerServiceSendNotificationResponse{Id: id.Secret(8)}), nil
}

// Events are what the service published.
func (h *Harness) Events() []PublishedEvent {
	h.mu.Lock()
	defer h.mu.Unlock()
	return append([]PublishedEvent{}, h.events...)
}

// Notifications are what the service sent.
func (h *Harness) Notifications() []SentNotification {
	h.mu.Lock()
	defer h.mu.Unlock()
	return append([]SentNotification{}, h.notifications...)
}

// DialNode reaches the target on this machine, through DialNode when the test set
// one, and carries bytes both ways.
func (s harnessServer) DialNode(ctx context.Context, stream *connect.BidiStream[pluginv1.ServerServiceDialNodeRequest, pluginv1.ServerServiceDialNodeResponse]) error {
	first, err := stream.Receive()
	if err != nil {
		return connect.NewError(connect.CodeInvalidArgument, fmt.Errorf("sdk: no target: %w", err))
	}
	target := Target{Node: first.GetNode(), TCPPort: first.GetTcpPort(), UnixPath: first.GetUnixPath()}
	if err := target.Validate(); err != nil {
		return connect.NewError(connect.CodeInvalidArgument, err)
	}
	s.harness.mu.Lock()
	s.harness.dials = append(s.harness.dials, target)
	dial := s.harness.DialNode
	s.harness.mu.Unlock()

	if dial == nil {
		dial = dialTarget
	}
	conn, err := dial(ctx, target)
	if err != nil {
		return connect.NewError(connect.CodeUnavailable, fmt.Errorf("sdk: reaching %s: %w", target, err))
	}
	defer func() { _ = conn.Close() }()

	if data := first.GetData(); len(data) > 0 {
		if _, err := conn.Write(data); err != nil {
			return connect.NewError(connect.CodeUnavailable, err)
		}
	}

	// When the caller goes, the connection to the node goes with it: the
	// goroutine below may be waiting in a read that nothing else ends.
	done := make(chan struct{})
	defer close(done)
	go func() {
		select {
		case <-ctx.Done():
			_ = conn.Close()
		case <-done:
		}
	}()

	// Either direction ending ends the connection, and the other is given a
	// moment to notice. The handler's return ends the stream in any case, so a
	// read that nothing else unblocks cannot hold the call open.
	ended := make(chan error, 2)
	go func() { ended <- streamToConn(ctx, stream, conn) }()
	go func() { ended <- connToStream(ctx, conn, stream) }()
	err = <-ended
	_ = conn.Close()
	select {
	case other := <-ended:
		if err == nil {
			err = other
		}
	case <-time.After(100 * time.Millisecond):
	}
	if err != nil && !errors.Is(err, io.EOF) && !errors.Is(err, net.ErrClosed) && !errors.Is(err, context.Canceled) {
		return connect.NewError(connect.CodeUnavailable, err)
	}
	return nil
}

// streamToConn carries the stream's bytes to the connection.
func streamToConn(ctx context.Context, stream *connect.BidiStream[pluginv1.ServerServiceDialNodeRequest, pluginv1.ServerServiceDialNodeResponse], conn net.Conn) error {
	for {
		if err := ctx.Err(); err != nil {
			return err
		}
		message, err := stream.Receive()
		if errors.Is(err, io.EOF) {
			if half, ok := conn.(interface{ CloseWrite() error }); ok {
				return half.CloseWrite()
			}
			return nil
		}
		if err != nil {
			return err
		}
		if data := message.GetData(); len(data) > 0 {
			if _, err := conn.Write(data); err != nil {
				return err
			}
		}
	}
}

// connToStream carries the connection's bytes to the stream.
func connToStream(ctx context.Context, conn net.Conn, stream *connect.BidiStream[pluginv1.ServerServiceDialNodeRequest, pluginv1.ServerServiceDialNodeResponse]) error {
	buffer := make([]byte, 32*1024)
	for {
		if err := ctx.Err(); err != nil {
			return err
		}
		n, err := conn.Read(buffer)
		if n > 0 {
			data := make([]byte, n)
			copy(data, buffer[:n])
			if sendErr := stream.Send(&pluginv1.ServerServiceDialNodeResponse{Data: data}); sendErr != nil {
				return sendErr
			}
		}
		if err != nil {
			if errors.Is(err, io.EOF) {
				return nil
			}
			return err
		}
	}
}

// dialTarget dials a target on this machine, which is what a test that points a
// node at a local engine wants.
func dialTarget(ctx context.Context, target Target) (net.Conn, error) {
	var dialer net.Dialer
	if target.UnixPath != "" {
		return dialer.DialContext(ctx, "unix", target.UnixPath)
	}
	return dialer.DialContext(ctx, "tcp", fmt.Sprintf("127.0.0.1:%d", target.TCPPort))
}

// lineWriter is an io.Writer for a child's output, which it hands to a sink a
// line at a time.
type lineWriter struct {
	sink func(string)
	mu   sync.Mutex
	rest bytes.Buffer
}

func (w *lineWriter) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	for {
		index := bytes.IndexByte(p, '\n')
		if index < 0 {
			w.rest.Write(p)
			return len(p), nil
		}
		w.rest.Write(p[:index])
		w.sink(strings.TrimRight(w.rest.String(), "\r"))
		w.rest.Reset()
		p = p[index+1:]
	}
}
