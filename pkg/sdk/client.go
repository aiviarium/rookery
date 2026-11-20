package sdk

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"path"
	"strconv"

	"connectrpc.com/connect"

	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/plugin/v1/pluginv1connect"
)

// TokenHeader is where a plugin presents ROOKERY_PLUGIN_TOKEN on the server's
// socket.
const TokenHeader = "Authorization"

// Client is a plugin's connection to the server's Unix socket.
type Client struct {
	config Config
	http   *http.Client
	server pluginv1connect.ServerServiceClient
}

// NewClient connects to the server's socket that config names. It does not call
// anything yet: a call fails when the server is not there.
func NewClient(config Config) (*Client, error) {
	if config.Socket == "" {
		return nil, fmt.Errorf("%w: no server socket", ErrConfig)
	}
	httpClient, base := socketClient(config.Socket)
	if config.Token != "" {
		httpClient.Transport = tokenTransport{token: config.Token, base: httpClient.Transport}
	}
	return &Client{
		config: config,
		http:   httpClient,
		server: pluginv1connect.NewServerServiceClient(httpClient, base, connect.WithGRPC()),
	}, nil
}

// Config is the configuration the client was made with.
func (c *Client) Config() Config { return c.config }

// HTTPClient is the client the server's socket is reached through.
func (c *Client) HTTPClient() *http.Client { return c.http }

// BaseURL is the address the server's socket is reached at, beside HTTPClient.
func (c *Client) BaseURL() string { return "http://" + path.Base(c.config.Socket) }

// Target is where a DialNode connects on a node: a port on the node's loopback,
// or the path of a Unix socket there. Exactly one of the two is set.
type Target struct {
	// Node is the managed host's name.
	Node string

	// TCPPort is a port on the node's loopback.
	TCPPort uint32

	// UnixPath is a Unix socket on the node, such as /var/run/docker.sock.
	UnixPath string
}

// String is how a target is written in a message.
func (t Target) String() string {
	switch {
	case t.UnixPath != "":
		return t.Node + ":" + t.UnixPath
	case t.TCPPort != 0:
		return t.Node + ":" + strconv.FormatUint(uint64(t.TCPPort), 10)
	default:
		return t.Node
	}
}

// Validate refuses a target that names no endpoint or more than one.
func (t Target) Validate() error {
	switch {
	case t.Node == "":
		return fmt.Errorf("%w: a target names the node to reach", ErrConfig)
	case t.UnixPath != "" && t.TCPPort != 0:
		return fmt.Errorf("%w: a target is a port or a Unix socket, not both", ErrConfig)
	case t.UnixPath == "" && t.TCPPort == 0:
		return fmt.Errorf("%w: a target names a port or a Unix socket", ErrConfig)
	}
	return nil
}

// DialNode opens a byte stream to a port on a node's loopback or to a Unix socket
// there, through the server's link to the node. The connection is a net.Conn:
// Close ends it, and CloseWrite half-closes it.
func (c *Client) DialNode(ctx context.Context, target Target) (net.Conn, error) {
	if err := target.Validate(); err != nil {
		return nil, err
	}
	ctx, cancel := context.WithCancel(ctx)
	stream := c.server.DialNode(ctx)
	if err := stream.Send(&pluginv1.ServerServiceDialNodeRequest{
		Node:     target.Node,
		TcpPort:  target.TCPPort,
		UnixPath: target.UnixPath,
	}); err != nil {
		cancel()
		return nil, fmt.Errorf("sdk: opening a stream to %s: %w", target, err)
	}
	conn := newNodeConn(target, stream, cancel)
	go conn.receive()
	return conn, nil
}

// Node returns a dialer that reaches addresses on one node through the server.
func (c *Client) Node(node string) *NodeDialer { return &NodeDialer{client: c, node: node} }

// NodeDialer is a net.Dialer-shaped dialer for one node: give it to a Docker
// client, an Incus client or an HTTP transport, and its dials land on the node's
// loopback or its Unix sockets.
type NodeDialer struct {
	client *Client
	node   string
}

// Node is the node's name.
func (d *NodeDialer) Node() string { return d.node }

// DialContext dials through the server. For "tcp", "tcp4" and "tcp6" the
// address's port is dialled on the node's loopback, and its host is ignored:
// nothing else on the node's network is reachable this way. For "unix" the
// address is a socket path on the node.
func (d *NodeDialer) DialContext(ctx context.Context, network, address string) (net.Conn, error) {
	switch network {
	case "tcp", "tcp4", "tcp6":
		_, port, err := net.SplitHostPort(address)
		if err != nil {
			return nil, fmt.Errorf("sdk: %q is not a host and port: %w", address, err)
		}
		number, err := strconv.ParseUint(port, 10, 32)
		if err != nil || number == 0 {
			return nil, fmt.Errorf("sdk: %q has no port to dial on the node's loopback", address)
		}
		return d.client.DialNode(ctx, Target{Node: d.node, TCPPort: uint32(number)})
	case "unix":
		return d.client.DialNode(ctx, Target{Node: d.node, UnixPath: address})
	default:
		return nil, fmt.Errorf("sdk: a node dialer reaches tcp or unix, not %q", network)
	}
}

// tokenTransport adds the plugin's token to every call.
type tokenTransport struct {
	token string
	base  http.RoundTripper
}

func (t tokenTransport) RoundTrip(request *http.Request) (*http.Response, error) {
	cloned := request.Clone(request.Context())
	cloned.Header.Set(TokenHeader, "Bearer "+t.token)
	return t.base.RoundTrip(cloned)
}

// socketClient returns an HTTP client and a base URL for a Unix socket that
// speaks gRPC, which is HTTP/2 without TLS between Rookery's own processes.
func socketClient(socket string) (*http.Client, string) {
	protocols := new(http.Protocols)
	protocols.SetUnencryptedHTTP2(true)
	transport := &http.Transport{
		Protocols: protocols,
		DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
			var dialer net.Dialer
			return dialer.DialContext(ctx, "unix", socket)
		},
	}
	return &http.Client{Transport: transport}, "http://" + path.Base(socket)
}

// DialSocket returns an HTTP client and a base URL for a Unix socket that serves
// gRPC, for a caller that builds a client of the services a plugin serves, such
// as NewProvisionerClient.
func DialSocket(socket string) (*http.Client, string) { return socketClient(socket) }
