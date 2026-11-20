// Package agent is rookeryd: the daemon on a managed host.
//
// It runs as root under systemd and has no database. Its only state is the
// channel store, a bbolt file. It listens on two Unix sockets:
//
//   - the root socket, mode 0600, which the server reaches through an SSH
//     session that runs `sudo -n rookeryd connect`. Every service, for any
//     account, is open to it.
//   - the user socket, mode 0666, which box and other local programs use. The
//     caller's uid comes from SO_PEERCRED, and a caller gets what that account
//     may do on the host: its own terminal sessions, its own channel topics, the
//     host services, the desktop when it is the desktop's account, and the
//     actions its features opened to users.
//
// Calls are gRPC in both directions, served over HTTP/2 in cleartext on the
// socket, so a client from connect-go with connect.WithGRPC() and a client from
// any other language's gRPC library talk to it the same way.
package agent

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"os"
	"runtime"
	"time"

	"golang.org/x/sync/errgroup"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/host/v1/hostv1connect"
	"github.com/aiviarium/rookery/internal/streamio"
)

// Where rookeryd's files are unless the flags say otherwise.
const (
	// RootSocketPath is the socket the server uses.
	RootSocketPath = "/run/rookeryd/root.sock"
	// UserSocketPath is the socket box and other local programs use.
	UserSocketPath = "/run/rookeryd/user.sock"
	// ChannelDBPath is the channel store.
	ChannelDBPath = "/var/lib/rookeryd/channel.db"
	// ActionsPath is the file of user actions the apply writes.
	ActionsPath = "/etc/rookery/actions.json"
	// DesktopEnvPath is the desktop's environment, which `box exec` and an exec
	// with Desktop set use.
	DesktopEnvPath = "/etc/rookery/desktop.env"

	// DefaultOperator is the account that runs operator work.
	DefaultOperator = "ops"
	// DefaultDesktopUser is the account that owns the desktop on a workstation.
	DefaultDesktopUser = "dev"
)

// Config is what a daemon is built from. It is read once, from flags and
// ROOKERYD_* variables, by cmd/rookeryd.
type Config struct {
	// Version is the build's version, reported by AgentService.Version.
	Version string

	// RootSocket and UserSocket are the two socket paths.
	RootSocket string
	UserSocket string

	// ChannelDB is the bbolt file the channel keeps its messages in. Empty
	// leaves the daemon without a channel, which only a test wants.
	ChannelDB string

	// ActionsPath is the JSON file of the host's user actions.
	ActionsPath string

	// DesktopEnvPath is the desktop's environment file.
	DesktopEnvPath string

	// Operator and DesktopUser are the accounts operator work and the desktop
	// belong to.
	Operator    string
	DesktopUser string

	// Log is the logger. Nil uses slog.Default.
	Log *slog.Logger
}

// Server is the daemon: its sockets, and the state the services keep.
type Server struct {
	config  Config
	log     *slog.Logger
	started time.Time

	root net.Listener
	user net.Listener

	mux      http.Handler
	sessions *sessionSet
	channels *channelStore
	registry *hostCallRegistry
	actions  *actionSet
}

// New builds a daemon and opens its sockets. Serve then answers on them.
//
// Opening the sockets here rather than in Serve is what lets a test know the
// daemon is reachable without polling: New returning means the sockets exist.
func New(ctx context.Context, config Config) (*Server, error) {
	if config.Version == "" {
		config.Version = "dev"
	}
	if config.RootSocket == "" || config.UserSocket == "" {
		return nil, errors.New("agent: both socket paths are required")
	}
	if config.ActionsPath == "" {
		config.ActionsPath = ActionsPath
	}
	if config.DesktopEnvPath == "" {
		config.DesktopEnvPath = DesktopEnvPath
	}
	if config.Operator == "" {
		config.Operator = DefaultOperator
	}
	if config.DesktopUser == "" {
		config.DesktopUser = DefaultDesktopUser
	}
	if config.Log == nil {
		config.Log = slog.Default()
	}

	s := &Server{
		config:  config,
		log:     config.Log,
		started: time.Now(),
	}

	var err error
	if config.ChannelDB != "" {
		if s.channels, err = openChannelStore(config.ChannelDB, defaultChannelLogLimit); err != nil {
			return nil, err
		}
	}

	s.sessions = newSessionSet()
	s.registry = newHostCallRegistry()
	s.actions = newActionSet(config.ActionsPath)

	root, err := listen(ctx, config.RootSocket, 0o600)
	if err != nil {
		s.closeState()
		return nil, err
	}
	s.root = root
	user, err := listen(ctx, config.UserSocket, 0o666)
	if err != nil {
		_ = s.root.Close()
		s.closeState()
		return nil, err
	}
	s.user = user

	s.mux = s.handlers()
	return s, nil
}

// Serve answers on both sockets until ctx is canceled. It then closes them and
// ends the sessions, and returns nil.
func (s *Server) Serve(ctx context.Context) error {
	group, ctx := errgroup.WithContext(ctx)
	group.Go(func() error { return s.serveSocket(ctx, s.root, roleRoot) })
	group.Go(func() error { return s.serveSocket(ctx, s.user, roleUser) })
	err := group.Wait()
	_ = s.Close()
	return err
}

// Close ends the sessions and closes the sockets and the channel store. It is
// safe after Serve and safe twice.
func (s *Server) Close() error {
	var errs []error
	if s.root != nil {
		errs = append(errs, s.root.Close())
	}
	if s.user != nil {
		errs = append(errs, s.user.Close())
	}
	errs = append(errs, s.sessions.closeAll())
	s.closeState()
	// A Unix socket survives its listener; removing it here means a restart does
	// not have to clean up after a crash.
	_ = os.Remove(s.config.RootSocket)
	_ = os.Remove(s.config.UserSocket)
	return errors.Join(errs...)
}

// closeState closes what New opened before the sockets.
func (s *Server) closeState() {
	if s.channels != nil {
		_ = s.channels.Close()
		s.channels = nil
	}
}

// RootSocket is the socket the server uses.
// RootSocket and UserSocket are the paths the daemon listens on.
func (s *Server) RootSocket() string { return s.config.RootSocket }

// UserSocket is the socket local programs use.
func (s *Server) UserSocket() string { return s.config.UserSocket }

// handlers is the one set of handlers both sockets share. Which socket a call
// came in on decides what its caller may do, and the handlers read that from the
// connection's context.
func (s *Server) handlers() http.Handler {
	mux := http.NewServeMux()
	register := func(path string, handler http.Handler) { mux.Handle(path, handler) }

	register(hostv1connect.NewAgentServiceHandler(&agentService{server: s}))
	register(hostv1connect.NewSessionServiceHandler(&sessionService{server: s}))
	register(hostv1connect.NewExecServiceHandler(&execService{server: s}))
	register(hostv1connect.NewFileServiceHandler(&fileService{}))
	portPath, portHandler := hostv1connect.NewPortServiceHandler(&portService{server: s})
	register(portPath, streamio.WithReadInterrupt(portHandler))
	register(hostv1connect.NewChannelServiceHandler(&channelService{server: s}))
	hostCallPath, hostCallHandler := hostv1connect.NewHostCallServiceHandler(&hostCallService{server: s})
	register(hostCallPath, streamio.WithReadInterrupt(hostCallHandler))
	register(hostv1connect.NewActionServiceHandler(&actionService{server: s}))
	register(hostv1connect.NewDesktopServiceHandler(&desktopService{server: s}))
	return mux
}

// serveSocket answers on one socket until ctx is canceled.
func (s *Server) serveSocket(ctx context.Context, listener net.Listener, role socketRole) error {
	server := &http.Server{
		Handler:   s.mux,
		Protocols: cleartextHTTP2(),
		// A local program that connects and says nothing must not hold a
		// connection forever without a call.
		ReadHeaderTimeout: 30 * time.Second,
		ConnContext: func(ctx context.Context, conn net.Conn) context.Context {
			return context.WithValue(ctx, callerKey{}, s.describe(conn, role))
		},
		BaseContext: func(net.Listener) context.Context { return ctx },
	}

	served := make(chan error, 1)
	go func() { served <- server.Serve(listener) }()

	select {
	case err := <-served:
		if err != nil && !errors.Is(err, net.ErrClosed) {
			return fmt.Errorf("agent: serving %s: %w", listener.Addr(), err)
		}
		return nil
	case <-ctx.Done():
		shutdown, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = server.Shutdown(shutdown) //nolint:contextcheck // the shutdown has a budget of its own, not the canceled call's.
		if err := <-served; err != nil && !errors.Is(err, net.ErrClosed) && !errors.Is(err, http.ErrServerClosed) {
			s.log.Warn("a socket stopped with an error while the daemon was stopping", "socket", listener.Addr().String(), "error", err)
		}
		return nil
	}
}

// describe reads who is on the other end of a connection and what the socket
// lets them do.
func (s *Server) describe(conn net.Conn, role socketRole) *caller {
	uid, gid, err := peerCredentials(conn)
	if err != nil {
		return &caller{role: role, err: fmt.Errorf("agent: reading the caller's credentials: %w", err)}
	}

	acct, err := lookupAccountByID(uid)
	if err != nil {
		return &caller{role: role, uid: uid, gid: gid, err: fmt.Errorf("agent: the caller's account: %w", err)}
	}
	if acct.GID == 0 {
		acct.GID = gid
	}

	if role == roleRoot && uid != 0 && uid != uint32(os.Geteuid()) { //nolint:gosec // a uid is a uint32.
		return &caller{
			role: role,
			uid:  uid,
			gid:  gid,
			err:  fmt.Errorf("agent: the root socket is for root, and uid %d is not", uid),
		}
	}
	return &caller{role: role, uid: uid, gid: gid, account: acct}
}

// version is the answer to AgentService.Version.
func (s *Server) version() *hostv1.AgentServiceVersionResponse {
	hostname, _ := os.Hostname()
	response := &hostv1.AgentServiceVersionResponse{
		Version:       s.config.Version,
		Os:            runtime.GOOS,
		Arch:          runtime.GOARCH,
		Hostname:      hostname,
		UptimeSeconds: uint64(time.Since(s.started).Seconds()), //nolint:gosec // a process does not run long enough to overflow.
	}
	if s.registry.has(DesktopProvider) {
		response.Desktop = &hostv1.DesktopState{Available: true, User: s.config.DesktopUser}
	}
	return response
}
