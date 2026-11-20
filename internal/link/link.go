// Package link is how the server reaches a host: the SSH key ring it logs in
// with, the host keys it trusts, and at most one SSH connection per host, whose
// session runs `sudo -n rookeryd connect` and carries gRPC between the server
// and the host's daemon.
//
// An SSH connection is opened on demand and closed after five idle minutes,
// except while something is using it. A host with a channel follower keeps its
// connection open. Only the leader holds links: a standby has none, and a link
// that a leader change outlives is closed with the link manager.
package link

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	"golang.org/x/crypto/ssh"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/agent"
	"github.com/aiviarium/rookery/internal/pipeconn"
	"github.com/aiviarium/rookery/internal/store"
)

// DefaultIdle is how long a link may sit unused before it is closed.
const DefaultIdle = 5 * time.Minute

// RookerydCommand is what a session runs to become the daemon's connection.
var RookerydCommand = "sudo -n rookeryd connect"

// Tailnet dials a host by its tailnet address, through the server's node.
type Tailnet interface {
	Dial(ctx context.Context, network, address string) (net.Conn, error)
}

// Target is where a host is and who the server logs in as: what a link needs
// that the hosts package knows. It is a structure rather than a lookup so that
// link does not import hosts, which uses link for first contact.
type Target struct {
	// Name is the host's name in Rookery.
	Name string
	// Address is the address to reach it at: its tailnet address when it has
	// one, its profile's otherwise.
	Address string
	// TailnetAddress is set when Address is on the tailnet, so the dialer is
	// used.
	TailnetAddress string
	// Port is the SSH port.
	Port int
	// SSHUser is the account the server logs in as.
	SSHUser string
}

// dialAddress is the address a link dials.
func (t Target) dialAddress() string {
	port := t.Port
	if port == 0 {
		port = 22
	}
	return net.JoinHostPort(t.Address, strconv.Itoa(port))
}

// knownAddress is the address the host key is trusted under.
func (t Target) knownAddress() string { return Address(t.Address, t.Port) }

// Targets looks a host's target up.
type Targets interface {
	Target(ctx context.Context, name string) (Target, error)
}

// TargetsFunc is a Target lookup as a function, for tests and wiring.
type TargetsFunc func(ctx context.Context, name string) (Target, error)

// Target looks the host up.
func (f TargetsFunc) Target(ctx context.Context, name string) (Target, error) {
	return f(ctx, name)
}

// Options are what a manager is built with.
type Options struct {
	// Targets looks a host up. Required.
	Targets Targets
	// Tailnet dials a host with a tailnet address. Nil dials directly.
	Tailnet Tailnet
	// Idle is how long an unused link stays open. Zero is DefaultIdle.
	Idle time.Duration
	// Log is the logger. Nil uses slog.Default.
	Log *slog.Logger
	// KeepOpen names hosts whose link is never closed for being idle, which are
	// the hosts whose channel a follower listens to.
	KeepOpen func(name string) bool
}

// Manager holds the links, at most one per host.
type Manager struct {
	opened  *store.Store
	ring    *Ring
	known   *KnownHosts
	options Options

	mu    sync.Mutex
	links map[string]*connection
}

// NewManager returns the link manager. It holds no link until one is asked
// for.
func NewManager(opened *store.Store, ring *Ring, known *KnownHosts, options Options) *Manager {
	if options.Idle == 0 {
		options.Idle = DefaultIdle
	}
	if options.Log == nil {
		options.Log = slog.Default()
	}
	return &Manager{opened: opened, ring: ring, known: known, options: options, links: make(map[string]*connection)}
}

// Ring is the key ring links log in with.
func (m *Manager) Ring() *Ring { return m.ring }

// KnownHosts is the host keys the server trusts.
func (m *Manager) KnownHosts() *KnownHosts { return m.known }

// Use returns the daemon client for a host, opening the link when there is
// none, and the function that lets it go idle again. A call holds its link while
// it runs, so the idle close never cuts one short.
func (m *Manager) Use(ctx context.Context, name string) (*agent.Client, func(), error) {
	conn, err := m.connect(ctx, name)
	if err != nil {
		return nil, nil, err
	}
	return conn.client, conn.release, nil
}

// connection is one host's link: the SSH connection, the session it runs the
// daemon in, and the daemon client on the session's pipe.
type connection struct {
	host string
	ssh  *ssh.Client
	sess *ssh.Session
	pipe net.Conn

	client *agent.Client

	// users counts the calls holding the link; a link with users is not idle.
	users   atomic.Int32
	lastUse atomic.Int64
	// closed is set once the connection is torn down.
	closed atomic.Bool
}

// connect returns the host's link, opening it if needed.
func (m *Manager) connect(ctx context.Context, name string) (*connection, error) {
	m.mu.Lock()
	conn := m.links[name]
	if conn != nil && !conn.closed.Load() {
		conn.touch()
		m.mu.Unlock()
		return conn, nil
	}
	m.mu.Unlock()

	// One dialer per host at a time: two callers must not open two links.
	m.mu.Lock()
	defer m.mu.Unlock()
	if conn = m.links[name]; conn != nil && !conn.closed.Load() {
		conn.touch()
		return conn, nil
	}
	opened, err := m.open(ctx, name)
	if err != nil {
		return nil, err
	}
	m.links[name] = opened
	go m.reap(opened)
	return opened, nil
}

// open makes a host's link: the SSH connection, the daemon session and the
// client on it.
func (m *Manager) open(ctx context.Context, name string) (*connection, error) {
	target, err := m.options.Targets.Target(ctx, name)
	if err != nil {
		return nil, err
	}
	if target.Address == "" {
		return nil, fmt.Errorf("%w: host %s has no address", access.ErrUnavailable, name)
	}
	key, err := m.ring.Active(ctx)
	if err != nil {
		return nil, err
	}

	client, err := m.dialSSH(ctx, target, ssh.PublicKeys(key.signer), m.known.Callback(ctx, target.knownAddress(), name))
	if err != nil {
		return nil, err
	}

	session, err := client.NewSession()
	if err != nil {
		_ = client.Close()
		return nil, fmt.Errorf("link: opening a session on %s: %w", name, err)
	}
	stdin, err := session.StdinPipe()
	if err != nil {
		_ = session.Close()
		_ = client.Close()
		return nil, fmt.Errorf("link: the session's input on %s: %w", name, err)
	}
	stdout, err := session.StdoutPipe()
	if err != nil {
		_ = session.Close()
		_ = client.Close()
		return nil, fmt.Errorf("link: the session's output on %s: %w", name, err)
	}
	if err := session.Start(RookerydCommand); err != nil {
		_ = session.Close()
		_ = client.Close()
		return nil, fmt.Errorf("%w: running rookeryd on %s: %w", access.ErrUnavailable, name, err)
	}

	pipe, err := pipeconn.New(pipeconn.Pair{Reader: io.NopCloser(stdout), Writer: stdin})
	if err != nil {
		_ = session.Close()
		_ = client.Close()
		return nil, fmt.Errorf("link: the session's pipe on %s: %w", name, err)
	}
	conn := &connection{
		host:   name,
		ssh:    client,
		sess:   session,
		pipe:   pipe,
		client: agent.NewClient(pipeconn.Client(pipe), nil),
	}
	conn.touch()
	m.options.Log.Info("opened a link", slog.String("host", name), slog.String("address", target.dialAddress()))
	return conn, nil
}

// touch records that the link was used, and holds it while the caller has it.
func (c *connection) touch() {
	c.lastUse.Store(time.Now().UnixNano())
	c.users.Add(1)
}

// release lets the link go idle.
func (c *connection) release() {
	if c.users.Add(-1) < 0 {
		c.users.Store(0)
	}
	c.lastUse.Store(time.Now().UnixNano())
}

// reap closes a link that has had no users for the idle time.
func (m *Manager) reap(conn *connection) {
	idle := m.options.Idle
	interval := idle / 10
	if interval < 100*time.Millisecond {
		interval = 100 * time.Millisecond
	}
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		if conn.closed.Load() {
			return
		}
		if m.options.KeepOpen != nil && m.options.KeepOpen(conn.host) {
			// A host whose channel is followed stays open however idle it is.
			conn.touch()
			conn.release()
			<-ticker.C
			continue
		}
		<-ticker.C
		if conn.closed.Load() {
			return
		}
		if conn.users.Load() > 0 {
			continue
		}
		if time.Since(time.Unix(0, conn.lastUse.Load())) < idle {
			continue
		}
		m.close(conn)
		return
	}
}

// close tears a link down and drops it from the manager.
func (m *Manager) close(conn *connection) {
	if conn.closed.Swap(true) {
		return
	}
	m.mu.Lock()
	if m.links[conn.host] == conn {
		delete(m.links, conn.host)
	}
	m.mu.Unlock()

	if conn.client != nil {
		_ = conn.client.Close()
	}
	if conn.pipe != nil {
		_ = conn.pipe.Close()
	}
	if conn.sess != nil {
		_ = conn.sess.Close()
	}
	if conn.ssh != nil {
		_ = conn.ssh.Close()
	}
	m.options.Log.Debug("closed an idle link", slog.String("host", conn.host))
}

// Close closes every link, as a stopping server does.
func (m *Manager) Close() {
	m.mu.Lock()
	links := make([]*connection, 0, len(m.links))
	for _, conn := range m.links {
		links = append(links, conn)
	}
	m.mu.Unlock()
	for _, conn := range links {
		m.close(conn)
	}
}

// Dial opens a raw SSH connection to a target for a caller that is not a link:
// first contact and an apply's key check.
func (m *Manager) dialSSH(ctx context.Context, target Target, auth ssh.AuthMethod, hostKey ssh.HostKeyCallback) (*ssh.Client, error) {
	address := target.dialAddress()
	var dialer net.Dialer
	conn, err := m.dial(ctx, &dialer, address, target)
	if err != nil {
		return nil, fmt.Errorf("%w: reaching %s: %w", access.ErrUnavailable, address, err)
	}

	user := target.SSHUser
	if user == "" {
		user = "ops"
	}
	config := &ssh.ClientConfig{
		User:            user,
		Auth:            []ssh.AuthMethod{auth},
		HostKeyCallback: hostKey,
		Timeout:         30 * time.Second,
	}
	clientConn, channels, requests, err := ssh.NewClientConn(conn, address, config)
	if err != nil {
		_ = conn.Close()
		return nil, fmt.Errorf("%w: logging in to %s: %w", access.ErrUnavailable, address, err)
	}
	return ssh.NewClient(clientConn, channels, requests), nil
}

// dial reaches a host's address: through the tailnet when it has one, directly
// otherwise.
func (m *Manager) dial(ctx context.Context, dialer *net.Dialer, address string, target Target) (net.Conn, error) {
	deadline := time.Now().Add(30 * time.Second)
	ctx, cancel := context.WithDeadline(ctx, deadline)
	defer cancel()

	if target.TailnetAddress != "" && m.options.Tailnet != nil {
		return m.options.Tailnet.Dial(ctx, "tcp", address)
	}
	return dialer.DialContext(ctx, "tcp", address)
}

// ErrNoTarget reports a host the target lookup does not know.
var ErrNoTarget = errors.New("link: no such host to link to")
