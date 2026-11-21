package link_test

import (
	"crypto/ed25519"
	"crypto/rand"
	"log/slog"
	"testing"

	"golang.org/x/crypto/ssh"

	"github.com/aiviarium/rookery/internal/link"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// newHostKey is a fresh SSH host key, as a machine made again would have.
func newHostKey(t *testing.T) ssh.PublicKey {
	t.Helper()
	public, _, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	key, err := ssh.NewPublicKey(public)
	if err != nil {
		t.Fatal(err)
	}
	return key
}

// TestForgetHost checks that a host's machine made again at the same address is
// trusted on first use once the host's keys are forgotten, and refused before.
func TestForgetHost(t *testing.T) {
	t.Parallel()
	ctx := t.Context()
	known := link.NewKnownHosts(storetest.New(t), slog.New(slog.DiscardHandler))

	const address = "192.0.2.10:22"
	if err := known.Check(ctx, address, "box", newHostKey(t)); err != nil {
		t.Fatalf("trusting the first key: %v", err)
	}
	if err := known.Check(ctx, "192.0.2.11:22", "other", newHostKey(t)); err != nil {
		t.Fatalf("trusting another host's key: %v", err)
	}

	again := newHostKey(t)
	if err := known.Check(ctx, address, "box", again); err == nil {
		t.Fatal("a changed key was trusted")
	}

	if err := known.ForgetHost(ctx, "box"); err != nil {
		t.Fatalf("forgetting the host: %v", err)
	}
	if err := known.Check(ctx, address, "box", again); err != nil {
		t.Fatalf("the machine made again is refused: %v", err)
	}
	if _, err := known.Line(ctx, "192.0.2.11:22"); err != nil {
		t.Fatalf("another host's key went too: %v", err)
	}
	if err := known.ForgetHost(ctx, "nothing"); err != nil {
		t.Fatalf("forgetting a host with no keys: %v", err)
	}
}
