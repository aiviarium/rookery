// Package linktest runs the SSH endpoint used by host integration tests.
package linktest

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"errors"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"

	"golang.org/x/crypto/ssh"

	"github.com/aiviarium/rookery/internal/agent"
	"github.com/aiviarium/rookery/internal/link"
)

// Password is the test account's initial credential.
const Password = "test-enrollment"

// SSH is an in-process SSH server for first contact and the rookeryd link.
type SSH struct {
	listener   net.Listener
	rootSocket string
	home       string
	config     *ssh.ServerConfig

	connections atomic.Int32
}

// Start opens a loopback SSH endpoint for the daemon's root socket.
func Start(t *testing.T, rootSocket string) *SSH {
	t.Helper()
	home := t.TempDir()
	_, private, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("generating the host key: %v", err)
	}
	signer, err := ssh.NewSignerFromKey(private)
	if err != nil {
		t.Fatalf("loading the host signer: %v", err)
	}
	config := &ssh.ServerConfig{
		PublicKeyCallback: func(_ ssh.ConnMetadata, key ssh.PublicKey) (*ssh.Permissions, error) {
			authorized, err := os.ReadFile(filepath.Join(home, ".ssh", "authorized_keys")) //nolint:gosec // the test's own directory
			if err != nil {
				return nil, err
			}
			for line := range strings.SplitSeq(string(authorized), "\n") {
				allowed, _, _, _, err := ssh.ParseAuthorizedKey([]byte(line))
				if err == nil && ssh.FingerprintSHA256(allowed) == ssh.FingerprintSHA256(key) {
					return nil, nil
				}
			}
			return nil, errors.New("key not authorized")
		},
		PasswordCallback: func(_ ssh.ConnMetadata, given []byte) (*ssh.Permissions, error) {
			if string(given) == Password {
				return nil, nil
			}
			return nil, errors.New("wrong password")
		},
	}
	config.AddHostKey(signer)
	ctx, cancel := context.WithCancel(t.Context())
	listener, err := (&net.ListenConfig{}).Listen(ctx, "tcp", "127.0.0.1:0")
	if err != nil {
		cancel()
		t.Fatalf("listening for SSH: %v", err)
	}
	server := &SSH{listener: listener, rootSocket: rootSocket, home: home, config: config}
	done := make(chan struct{})
	go func() {
		defer close(done)
		server.serve(ctx)
	}()
	t.Cleanup(func() {
		cancel()
		_ = listener.Close()
		<-done
	})
	return server
}

// Address returns the loopback host and port independently, as HostService.Add expects them.
func (s *SSH) Address() (string, int) {
	address := s.listener.Addr().(*net.TCPAddr)
	return address.IP.String(), address.Port
}

// Connections counts the TCP connections accepted by the fixture.
func (s *SSH) Connections() int32 { return s.connections.Load() }

// AuthorizedKeys returns the path where first contact installs the server's key.
func (s *SSH) AuthorizedKeys() string { return filepath.Join(s.home, ".ssh", "authorized_keys") }

func (s *SSH) serve(ctx context.Context) {
	var connections sync.WaitGroup
	defer connections.Wait()
	for {
		conn, err := s.listener.Accept()
		if err != nil {
			return
		}
		s.connections.Add(1)
		connections.Go(func() { s.handle(ctx, conn) })
	}
}

func (s *SSH) handle(ctx context.Context, conn net.Conn) {
	defer func() { _ = conn.Close() }()
	stop := context.AfterFunc(ctx, func() { _ = conn.Close() })
	defer stop()
	sshConn, channels, requests, err := ssh.NewServerConn(conn, s.config)
	if err != nil {
		return
	}
	var children sync.WaitGroup
	defer children.Wait()
	defer func() { _ = sshConn.Close() }()
	children.Go(func() { ssh.DiscardRequests(requests) })
	for channel := range channels {
		if channel.ChannelType() != "session" {
			_ = channel.Reject(ssh.UnknownChannelType, "only sessions")
			continue
		}
		accepted, channelRequests, err := channel.Accept()
		if err != nil {
			return
		}
		children.Go(func() { s.session(ctx, accepted, channelRequests) })
	}
}

func (s *SSH) session(ctx context.Context, channel ssh.Channel, requests <-chan *ssh.Request) {
	defer func() { _ = channel.Close() }()
	for request := range requests {
		var payload struct{ Command string }
		if request.Type != "exec" || ssh.Unmarshal(request.Payload, &payload) != nil {
			_ = request.Reply(false, nil)
			continue
		}
		_ = request.Reply(true, nil)
		var err error
		switch payload.Command {
		case link.RookerydCommand:
			err = agent.Connect(ctx, s.rootSocket, channel, channel)
		case link.InstallScript:
			command := exec.CommandContext(ctx, "sh", "-c", link.InstallScript) //nolint:gosec // the command is the constant fixture script.
			command.Env = append(os.Environ(), "HOME="+s.home)
			command.Stdin, command.Stderr = channel, channel.Stderr()
			err = command.Run()
		case "true":
		default:
			err = errors.New("unsupported SSH fixture command")
		}
		var status uint32
		if err != nil {
			status = 1
		}
		_, _ = channel.SendRequest("exit-status", false, ssh.Marshal(struct{ Status uint32 }{Status: status}))
		_ = channel.CloseWrite()
		return
	}
}
