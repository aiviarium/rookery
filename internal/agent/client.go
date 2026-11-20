package agent

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"

	"connectrpc.com/connect"

	hostv1connect "github.com/aiviarium/rookery/gen/go/rookery/host/v1/hostv1connect"
)

// Client is the daemon's services on one of its sockets.
//
// It speaks gRPC over HTTP/2 in cleartext, as the server and box do: every call
// is made with connect.WithGRPC(), so a program in another language reaches the
// same handlers with its own gRPC client.
type Client struct {
	http      *http.Client
	transport *http.Transport

	Agent     hostv1connect.AgentServiceClient
	Sessions  hostv1connect.SessionServiceClient
	Exec      hostv1connect.ExecServiceClient
	Files     hostv1connect.FileServiceClient
	Ports     hostv1connect.PortServiceClient
	Channel   hostv1connect.ChannelServiceClient
	HostCalls hostv1connect.HostCallServiceClient
	Actions   hostv1connect.ActionServiceClient
	Desktop   hostv1connect.DesktopServiceClient
}

// Dial makes a client for a socket. It checks that the socket is there, so a
// missing rookeryd is one error here rather than one per call.
func Dial(socket string) (*Client, error) {
	if socket == "" {
		return nil, errors.New("agent: no socket path")
	}
	if _, err := os.Stat(socket); err != nil {
		return nil, fmt.Errorf("agent: the socket %s: %w", socket, err)
	}

	transport := &http.Transport{
		Protocols: cleartextHTTP2(),
		// A Unix socket is already addressed: the address in the URL names
		// nothing.
		DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
			dialer := net.Dialer{}
			return dialer.DialContext(ctx, "unix", socket)
		},
		// gRPC frames its own messages; the transport must not decode them.
		DisableCompression: true,
	}
	client := &http.Client{Transport: transport}
	options := []connect.ClientOption{connect.WithGRPC()}
	// The socket is the address; the URL's host only has to be a host.
	const baseURL = "http://rookeryd"

	return &Client{
		http:      client,
		transport: transport,
		Agent:     hostv1connect.NewAgentServiceClient(client, baseURL, options...),
		Sessions:  hostv1connect.NewSessionServiceClient(client, baseURL, options...),
		Exec:      hostv1connect.NewExecServiceClient(client, baseURL, options...),
		Files:     hostv1connect.NewFileServiceClient(client, baseURL, options...),
		Ports:     hostv1connect.NewPortServiceClient(client, baseURL, options...),
		Channel:   hostv1connect.NewChannelServiceClient(client, baseURL, options...),
		HostCalls: hostv1connect.NewHostCallServiceClient(client, baseURL, options...),
		Actions:   hostv1connect.NewActionServiceClient(client, baseURL, options...),
		Desktop:   hostv1connect.NewDesktopServiceClient(client, baseURL, options...),
	}, nil
}

// Close ends the client's idle connections.
func (c *Client) Close() error {
	c.transport.CloseIdleConnections()
	return nil
}

// Connect bridges in and out to the daemon's root socket, which is what
// `rookeryd connect` does: the server runs it in an SSH session, and the
// session's stdin and stdout become a connection rookeryd answers on.
func Connect(ctx context.Context, socket string, in io.Reader, out io.Writer) error {
	dialer := net.Dialer{}
	conn, err := dialer.DialContext(ctx, "unix", socket)
	if err != nil {
		return fmt.Errorf("agent: dialing %s: %w", socket, err)
	}
	defer func() { _ = conn.Close() }()

	done := make(chan error, 2)
	go func() {
		_, err := io.Copy(conn, in)
		// The server's end of the session is done sending: the half close is
		// what tells rookeryd so, and reading stays open for the answer.
		if closer, ok := conn.(interface{ CloseWrite() error }); ok {
			_ = closer.CloseWrite()
		}
		done <- err
	}()
	go func() {
		_, err := io.Copy(out, conn)
		done <- err
	}()

	select {
	case err := <-done:
		if err != nil && !errors.Is(err, io.EOF) && !errors.Is(err, net.ErrClosed) {
			return fmt.Errorf("agent: bridging %s: %w", socket, err)
		}
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}
