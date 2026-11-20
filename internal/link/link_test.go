package link_test

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"connectrpc.com/connect"
	"golang.org/x/crypto/ssh"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"

	"github.com/aiviarium/rookery/internal/agent"
	"github.com/aiviarium/rookery/internal/link"
	"github.com/aiviarium/rookery/internal/link/linktest"
	"github.com/aiviarium/rookery/internal/seal"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

func startDaemon(t *testing.T) string {
	t.Helper()

	dir := t.TempDir()
	ctx, cancel := context.WithCancel(t.Context())
	server, err := agent.New(ctx, agent.Config{
		Version:    "test",
		RootSocket: filepath.Join(dir, "root.sock"),
		UserSocket: filepath.Join(dir, "user.sock"),
	})
	if err != nil {
		cancel()
		t.Fatalf("starting rookeryd: %v", err)
	}
	done := make(chan error, 1)
	go func() { done <- server.Serve(ctx) }()
	t.Cleanup(func() {
		cancel()
		if err := <-done; err != nil {
			t.Errorf("stopping rookeryd: %v", err)
		}
	})
	return server.RootSocket()
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
	server := linktest.Start(t, d)
	host, port := server.Address()
	target := link.Target{Name: "forge", Address: host, Port: port, SSHUser: "ops"}
	m := manager(t, opened, target)

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	contact, err := m.Contact(ctx, target, link.Credential{Password: linktest.Password}, testLogger(t))
	if err != nil {
		t.Fatalf("Contact() = %v", err)
	}
	if !contact.Installed || contact.KeyWorked {
		t.Errorf("first contact = %+v, want the key installed", contact)
	}
	if !strings.HasPrefix(contact.Fingerprint, "SHA256:") {
		t.Errorf("fingerprint = %q, want a SHA-256 fingerprint", contact.Fingerprint)
	}
	authorized, err := os.ReadFile(server.AuthorizedKeys())
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
	server := linktest.Start(t, startDaemon(t))
	host, port := server.Address()
	target := link.Target{Name: "forge", Address: host, Port: port, SSHUser: "ops"}
	m := manager(t, opened, target)

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	if _, err := m.Contact(ctx, target, link.Credential{Password: linktest.Password}, testLogger(t)); err != nil {
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
	server := linktest.Start(t, d)
	host, port := server.Address()
	target := link.Target{Name: "forge", Address: host, Port: port, SSHUser: "ops"}
	m := manager(t, opened, target)

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	if _, err := m.Contact(ctx, target, link.Credential{Password: linktest.Password}, testLogger(t)); err != nil {
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
	before := server.Connections()
	client, release, err = m.Use(ctx, "forge")
	if err != nil {
		t.Fatalf("second Use() = %v", err)
	}
	if _, err := client.Agent.Version(ctx, connectRequest()); err != nil {
		release()
		t.Fatalf("second Version() = %v", err)
	}
	release()
	if after := server.Connections(); after != before {
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
