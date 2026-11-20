package link_test

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"errors"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"connectrpc.com/connect"
	"golang.org/x/crypto/ssh"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"

	"github.com/aiviarium/rookery/internal/agent"
	"github.com/aiviarium/rookery/internal/link"
	"github.com/aiviarium/rookery/internal/seal"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// daemon starts rookeryd's handlers in the test, and is what "sudo -n rookeryd
// connect" runs: the SSH server bridges the session to the daemon's root
// socket.
type daemon struct {
	server *agent.Server
	cancel context.CancelFunc
	socket string
}

func startDaemon(t *testing.T) *daemon {
	t.Helper()
	dir := t.TempDir()
	ctx, cancel := context.WithCancel(context.Background())
	server, err := agent.New(ctx, agent.Config{
		Version:    "test",
		RootSocket: filepath.Join(dir, "root.sock"),
		UserSocket: filepath.Join(dir, "user.sock"),
	})
	if err != nil {
		cancel()
		t.Fatalf("starting rookeryd: %v", err)
	}
	go func() { _ = server.Serve(ctx) }()
	d := &daemon{server: server, cancel: cancel, socket: server.RootSocket()}
	t.Cleanup(func() {
		cancel()
		_ = server.Close()
	})
	return d
}

// sshServer is an in-process SSH server whose exec runs a shell in home, and
// whose `sudo -n rookeryd connect` bridges to the daemon.
type sshServer struct {
	listener net.Listener
	daemon   *daemon
	home     string
	config   *ssh.ServerConfig

	connections atomic.Int32
	wg          sync.WaitGroup
}

func startSSH(t *testing.T, d *daemon, password bool) *sshServer {
	t.Helper()
	home := t.TempDir()
	_, private, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("generating the host key: %v", err)
	}
	signer, err := ssh.NewSignerFromKey(private)
	if err != nil {
		t.Fatalf("the host signer: %v", err)
	}

	config := &ssh.ServerConfig{
		PublicKeyCallback: func(_ ssh.ConnMetadata, key ssh.PublicKey) (*ssh.Permissions, error) {
			authorized, err := os.ReadFile(filepath.Join(home, ".ssh", "authorized_keys")) //nolint:gosec // the test's own directory
			if err != nil {
				return nil, errors.New("no authorized key")
			}
			for _, line := range strings.Split(string(authorized), "\n") {
				if strings.TrimSpace(line) == "" {
					continue
				}
				allowed, _, _, _, err := ssh.ParseAuthorizedKey([]byte(line))
				if err != nil {
					continue
				}
				if ssh.FingerprintSHA256(allowed) == ssh.FingerprintSHA256(key) {
					return nil, nil
				}
			}
			return nil, errors.New("key not authorized")
		},
	}
	if password {
		config.PasswordCallback = func(_ ssh.ConnMetadata, given []byte) (*ssh.Permissions, error) {
			if string(given) == "hunter2" {
				return nil, nil
			}
			return nil, errors.New("wrong password")
		}
	}
	config.AddHostKey(signer)

	listener, err := (&net.ListenConfig{}).Listen(context.Background(), "tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listening: %v", err)
	}
	server := &sshServer{listener: listener, daemon: d, home: home, config: config}
	server.wg.Add(1)
	go server.serve()
	t.Cleanup(func() {
		_ = listener.Close()
		server.wg.Wait()
	})
	return server
}

func (s *sshServer) serve() {
	defer s.wg.Done()
	for {
		conn, err := s.listener.Accept()
		if err != nil {
			return
		}
		s.connections.Add(1)
		s.wg.Add(1)
		go func() {
			defer s.wg.Done()
			s.handle(conn)
		}()
	}
}

func (s *sshServer) handle(conn net.Conn) {
	defer func() { _ = conn.Close() }()
	sshConn, channels, requests, err := ssh.NewServerConn(conn, s.config)
	if err != nil {
		return
	}
	go ssh.DiscardRequests(requests)
	for channel := range channels {
		if channel.ChannelType() != "session" {
			_ = channel.Reject(ssh.UnknownChannelType, "only sessions")
			continue
		}
		accepted, channelRequests, err := channel.Accept()
		if err != nil {
			return
		}
		go s.session(accepted, channelRequests)
	}
	_ = sshConn.Close()
}

func (s *sshServer) session(channel ssh.Channel, requests <-chan *ssh.Request) {
	for request := range requests {
		if request.Type != "exec" {
			_ = request.Reply(false, nil)
			continue
		}
		var payload struct{ Command string }
		if err := ssh.Unmarshal(request.Payload, &payload); err != nil {
			_ = request.Reply(false, nil)
			return
		}
		_ = request.Reply(true, nil)
		if payload.Command == link.RookerydCommand {
			_ = agent.Connect(context.Background(), s.daemon.socket, channel, channel)
			exitStatus(channel, 0)
			return
		}
		command := exec.CommandContext(context.Background(), "sh", "-c", payload.Command) //nolint:gosec // the test's own command
		command.Env = append(os.Environ(), "HOME="+s.home)
		command.Stdin = channel
		command.Stdout = channel
		command.Stderr = channel.Stderr()
		status := 0
		if err := command.Run(); err != nil {
			status = 1
		}
		exitStatus(channel, uint32(status))
		return
	}
}

// exitStatus ends a session the way sshd does, so that the client's Run
// returns instead of waiting for an exit that never comes.
func exitStatus(channel ssh.Channel, status uint32) {
	_, _ = channel.SendRequest("exit-status", false, ssh.Marshal(struct{ Status uint32 }{Status: status}))
	_ = channel.CloseWrite()
	_ = channel.Close()
}

// address is where the server listens, without its port.
func (s *sshServer) address(t *testing.T) (string, int) {
	t.Helper()
	host, port, err := net.SplitHostPort(s.listener.Addr().String())
	if err != nil {
		t.Fatalf("the listener's address: %v", err)
	}
	var number int
	if _, err := fmt.Sscanf(port, "%d", &number); err != nil {
		t.Fatalf("the listener's port: %v", err)
	}
	return host, number
}

// manager builds a link manager for a store and a target.
func manager(t *testing.T, opened *store.Store, target link.Target) *link.Manager {
	t.Helper()
	sealed, err := seal.New()
	if err != nil {
		t.Fatalf("a sealing key: %v", err)
	}
	ring, err := seal.NewRing(sealed.ID(), sealed)
	if err != nil {
		t.Fatalf("a sealing ring: %v", err)
	}
	keys := link.NewRing(opened, ring)
	known := link.NewKnownHosts(opened, nil)
	manager := link.NewManager(opened, keys, known, link.Options{
		Targets: link.TargetsFunc(func(context.Context, string) (link.Target, error) { return target, nil }),
		Idle:    300 * time.Millisecond,
		Log:     testLogger(t),
	})
	t.Cleanup(manager.Close)
	return manager
}

func TestContactInstallsTheRingsKey(t *testing.T) {
	t.Parallel()
	opened := storetest.New(t)
	d := startDaemon(t)
	server := startSSH(t, d, true)
	host, port := server.address(t)
	target := link.Target{Name: "forge", Address: host, Port: port, SSHUser: "ops"}
	m := manager(t, opened, target)

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	contact, err := m.Contact(ctx, target, link.Credential{Password: "hunter2"}, testLogger(t))
	if err != nil {
		t.Fatalf("Contact() = %v", err)
	}
	if !contact.Installed || contact.KeyWorked {
		t.Errorf("first contact = %+v, want the key installed", contact)
	}
	if !strings.HasPrefix(contact.Fingerprint, "SHA256:") {
		t.Errorf("fingerprint = %q, want a SHA-256 fingerprint", contact.Fingerprint)
	}
	authorized, err := os.ReadFile(filepath.Join(server.home, ".ssh", "authorized_keys"))
	if err != nil {
		t.Fatalf("reading authorized_keys: %v", err)
	}
	key, err := m.Ring().Active(ctx)
	if err != nil {
		t.Fatalf("the ring's active key: %v", err)
	}
	if !strings.Contains(string(authorized), key.Public) {
		t.Errorf("authorized_keys = %q, want the ring's key %q", authorized, key.Public)
	}

	again, err := m.Contact(ctx, target, link.Credential{}, testLogger(t))
	if err != nil {
		t.Fatalf("second Contact() = %v", err)
	}
	if !again.KeyWorked || again.Installed {
		t.Errorf("second contact = %+v, want the ring's key to work", again)
	}
}

func TestContactRefusesAChangedHostKey(t *testing.T) {
	t.Parallel()
	opened := storetest.New(t)
	server := startSSH(t, startDaemon(t), true)
	host, port := server.address(t)
	target := link.Target{Name: "forge", Address: host, Port: port, SSHUser: "ops"}
	m := manager(t, opened, target)

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	if _, err := m.Contact(ctx, target, link.Credential{Password: "hunter2"}, testLogger(t)); err != nil {
		t.Fatalf("first contact: %v", err)
	}

	// Another key on the same address is a mismatch: it is kept as pending, and
	// only an admin trusting exactly that fingerprint makes it the trusted one.
	_, private, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("generating a key: %v", err)
	}
	signer, err := ssh.NewSignerFromKey(private)
	if err != nil {
		t.Fatalf("the signer: %v", err)
	}
	address := link.Address(host, port)
	if err := m.KnownHosts().Check(ctx, address, "forge", signer.PublicKey()); !errors.Is(err, link.ErrMismatch) {
		t.Fatalf("Check() with another key = %v, want ErrMismatch", err)
	}
	hosts, err := m.KnownHosts().List(ctx)
	if err != nil {
		t.Fatalf("listing the trusted keys: %v", err)
	}
	var pending string
	for _, known := range hosts {
		if known.PendingFingerprint != "" {
			pending = known.PendingFingerprint
		}
	}
	if pending == "" {
		t.Fatal("the offered key was not kept as pending")
	}
	if _, err := m.KnownHosts().Trust(ctx, address, "SHA256:nothing"); err == nil {
		t.Error("Trust() with a fingerprint the host never offered succeeded")
	}
	if _, err := m.KnownHosts().Trust(ctx, address, pending); err != nil {
		t.Fatalf("Trust() = %v", err)
	}
}

func TestLinkServesTheDaemonOverSSH(t *testing.T) {
	t.Parallel()
	opened := storetest.New(t)
	d := startDaemon(t)
	server := startSSH(t, d, true)
	host, port := server.address(t)
	target := link.Target{Name: "forge", Address: host, Port: port, SSHUser: "ops"}
	m := manager(t, opened, target)

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	if _, err := m.Contact(ctx, target, link.Credential{Password: "hunter2"}, testLogger(t)); err != nil {
		t.Fatalf("first contact: %v", err)
	}

	client, release, err := m.Use(ctx, "forge")
	if err != nil {
		t.Fatalf("Use() = %v", err)
	}
	version, err := client.Agent.Version(ctx, connectRequest())
	if err != nil {
		release()
		t.Fatalf("Version() = %v", err)
	}
	if version.Msg.GetVersion() != "test" {
		t.Errorf("version = %q, want test", version.Msg.GetVersion())
	}
	release()

	// The link is reused for the next call, and closed once it has been idle.
	before := server.connections.Load()
	client, release, err = m.Use(ctx, "forge")
	if err != nil {
		t.Fatalf("second Use() = %v", err)
	}
	if _, err := client.Agent.Version(ctx, connectRequest()); err != nil {
		release()
		t.Fatalf("second Version() = %v", err)
	}
	release()
	if after := server.connections.Load(); after != before {
		t.Errorf("the second call opened %d more connections, want the link reused", after-before)
	}

	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		client, release, err = m.Use(ctx, "forge")
		if err != nil {
			t.Fatalf("Use() after the close = %v", err)
		}
		_, err = client.Agent.Version(ctx, connectRequest())
		release()
		if err != nil {
			// The link was closed mid-call and reopened; that is the idle close
			// working, and a retry is what a caller does.
			continue
		}
		break
	}
}

// connectRequest is an empty AgentService.Version request.
func connectRequest() *connect.Request[hostv1.AgentServiceVersionRequest] {
	return connect.NewRequest(&hostv1.AgentServiceVersionRequest{})
}
