package tailnet

import (
	"context"
	"fmt"
	"log/slog"
	"net"

	"tailscale.com/tsnet"
)

// NodeOptions configure the server's own node in the tailnet.
type NodeOptions struct {
	// Store keeps the node's state. It is required: without it a standby could
	// not take the same identity.
	Store Store
	// Dir is where tsnet keeps its cache and its logs, under the state
	// directory.
	Dir string
	// Hostname is the name the server has on the tailnet.
	Hostname string
	// ControlURL is the control server's address; empty is Tailscale's own.
	ControlURL string
	// AuthKey joins the tailnet the first time. Once the store has the node's
	// state it is not needed, and never used again.
	AuthKey string
	// Log takes the node's logs, which slog discards when nil.
	Log *slog.Logger
	// UserLog takes what a person should see, such as the sign-in address.
	UserLog *slog.Logger
}

// Node is the server's node in the tailnet: it dials the hosts at their tailnet
// addresses and listens for clients. It is a Dialer.
type Node struct {
	server *tsnet.Server
}

// NewNode makes the server's node. Nothing happens on the tailnet until Start.
func NewNode(options NodeOptions) (*Node, error) {
	if options.Store == nil {
		return nil, fmt.Errorf("tailnet: the server's node needs a state store")
	}
	if options.Dir == "" {
		return nil, fmt.Errorf("tailnet: the server's node needs a directory")
	}
	log := options.Log
	if log == nil {
		log = slog.New(slog.DiscardHandler)
	}
	userLog := options.UserLog
	if userLog == nil {
		userLog = log
	}

	server := &tsnet.Server{
		Dir:        options.Dir,
		Store:      NewStateStore(options.Store),
		Hostname:   options.Hostname,
		ControlURL: options.ControlURL,
		AuthKey:    options.AuthKey,
		Logf: func(format string, args ...any) {
			log.Debug(fmt.Sprintf(format, args...), "tailnet", "node")
		},
		UserLogf: func(format string, args ...any) {
			userLog.Info(fmt.Sprintf(format, args...), "tailnet", "node")
		},
	}
	return &Node{server: server}, nil
}

// Start joins the tailnet and waits until the node is up. A node whose state the
// store already has needs no auth key; one that has none and was given a key
// joins with it.
func (n *Node) Start(ctx context.Context) error {
	if err := n.server.Start(); err != nil {
		return fmt.Errorf("tailnet: starting the server's node: %w", err)
	}
	if _, err := n.server.Up(ctx); err != nil {
		return fmt.Errorf("tailnet: joining the tailnet: %w", err)
	}
	return nil
}

// Dial opens a connection to a tailnet address. It is what links reach hosts
// with, and what `rookery server dial` runs for Ansible's SSH.
func (n *Node) Dial(ctx context.Context, network, address string) (net.Conn, error) {
	conn, err := n.server.Dial(ctx, network, address)
	if err != nil {
		return nil, fmt.Errorf("tailnet: dialing %s: %w", address, err)
	}
	return conn, nil
}

// Listen listens on the tailnet, for ROOKERY_TAILNET_LISTEN.
func (n *Node) Listen(network, address string) (net.Listener, error) {
	listener, err := n.server.Listen(network, address)
	if err != nil {
		return nil, fmt.Errorf("tailnet: listening on %s: %w", address, err)
	}
	return listener, nil
}

// Addresses are the node's own tailnet addresses, IPv4 first.
func (n *Node) Addresses() ([]string, error) {
	ip4, ip6 := n.server.TailscaleIPs()
	addresses := make([]string, 0, 2)
	if ip4.IsValid() {
		addresses = append(addresses, ip4.String())
	}
	if ip6.IsValid() {
		addresses = append(addresses, ip6.String())
	}
	return addresses, nil
}

// Close leaves the tailnet and stops the node.
func (n *Node) Close() error {
	if err := n.server.Close(); err != nil {
		return fmt.Errorf("tailnet: closing the server's node: %w", err)
	}
	return nil
}
