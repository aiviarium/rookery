package provision_test

import (
	"errors"
	"strings"
	"testing"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/provision"
)

// TestRenderSeed checks the cloud-init user data a machine starts with.
func TestRenderSeed(t *testing.T) {
	t.Parallel()

	tests := map[string]struct {
		seed   provision.Seed
		wants  []string
		avoids []string
	}{
		"an operator with the key and sudo": {
			seed: provision.Seed{Host: "desk-1", PublicKey: "ssh-ed25519 AAAAtest rookery"},
			wants: []string{
				"#cloud-config",
				"hostname: desk-1",
				"name: ops",
				"sudo: ALL=(ALL) NOPASSWD:ALL",
				"- ssh-ed25519 AAAAtest rookery",
				"ssh_pwauth: false",
				"datasource_list: [NoCloud, None]",
			},
			avoids: []string{"runcmd", "tailscale"},
		},
		"a tailnet key joins it": {
			seed: provision.Seed{Host: "desk-2", PublicKey: "ssh-ed25519 AAAAtest rookery", TailnetKey: "tskey-test-2"},
			wants: []string{
				"--auth-key=file:/run/rookery-tailnet-key", "content: tskey-test-2", "--hostname=desk-2",
				"https://tailscale.com/install.sh",
			},
		},
		"the key ring's key, ended by a newline as ssh writes it": {
			seed:  provision.Seed{Host: "desk-4", PublicKey: "ssh-ed25519 AAAAtest\n"},
			wants: []string{"- ssh-ed25519 AAAAtest\n"},
		},
		"another operator account": {
			seed:  provision.Seed{Host: "desk-3", Operator: "admin", PublicKey: "ssh-ed25519 AAAAtest rookery"},
			wants: []string{"name: admin"},
		},
	}
	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			seed, err := provision.RenderSeed(test.seed)
			if err != nil {
				t.Fatalf("rendering the seed: %v", err)
			}
			for _, want := range test.wants {
				if !strings.Contains(seed, want) {
					t.Errorf("the seed does not have %q:\n%s", want, seed)
				}
			}
			for _, avoid := range test.avoids {
				if strings.Contains(seed, avoid) {
					t.Errorf("the seed has %q:\n%s", avoid, seed)
				}
			}
		})
	}
}

// TestRenderSeedRefuses checks the seeds a machine could not come up from.
func TestRenderSeedRefuses(t *testing.T) {
	t.Parallel()

	tests := map[string]provision.Seed{
		"no host":            {PublicKey: "ssh-ed25519 AAAAtest rookery"},
		"no key":             {Host: "desk-1"},
		"a key with a line":  {Host: "desk-1", PublicKey: "ssh-ed25519 AAAAtest rookery\nssh-ed25519 BBB"},
		"a host with space":  {Host: "desk 1", PublicKey: "ssh-ed25519 AAAAtest rookery"},
		"an operator name":   {Host: "desk-1", Operator: "Ops!", PublicKey: "ssh-ed25519 AAAAtest rookery"},
		"a key with a space": {Host: "desk-1", PublicKey: "ssh-ed25519 AAAAtest rookery", TailnetKey: "tskey test"},
	}
	for name, seed := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			if _, err := provision.RenderSeed(seed); !errors.Is(err, access.ErrInvalid) {
				t.Fatalf("rendering %+v gave %v", seed, err)
			}
		})
	}
}
