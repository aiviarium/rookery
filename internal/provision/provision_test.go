package provision_test

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/provision"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/pkg/sdk"
)

// fakeBinary builds the example plugin's fake provisioner, so that a test starts
// a real one the way the server does. `go build` caches it between tests.
func fakeBinary(t *testing.T) string {
	t.Helper()

	binary := filepath.Join(t.TempDir(), "fake-provisioner")
	//nolint:gosec // the test builds the fixture it then runs.
	build := exec.CommandContext(t.Context(), "go", "build", "-o", binary, "./testdata/plugins/example/provisioner")
	build.Dir = "../.."
	if output, err := build.CombinedOutput(); err != nil {
		t.Fatalf("building the fake provisioner: %v\n%s", err, output)
	}
	return binary
}

// secretsOf is a deployment's provisioner secrets, by entry and name.
type secretsOf map[string]map[string]string

func (s secretsOf) ProvisionerSecrets(_ context.Context, entry string, names []string) (map[string]string, error) {
	out := map[string]string{}
	for _, name := range names {
		if value, ok := s[entry][name]; ok {
			out[name] = value
		}
	}
	return out, nil
}

// tokensOf mints one token, and remembers which plugin it was for.
type tokensOf struct {
	mu     sync.Mutex
	plugin string
}

func (t *tokensOf) PluginToken(_ context.Context, plugin string) (string, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.plugin = plugin
	return "rkplug_test", nil
}

// hostsOf records the machines a provisioner made.
type hostsOf struct {
	mu       sync.Mutex
	machines []provision.MachineHost
}

func (h *hostsOf) AddMachine(_ context.Context, machine provision.MachineHost) error {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.machines = append(h.machines, machine)
	return nil
}

// deployment is a test's provisioners, wired to the fake.
type deployment struct {
	provisioners *provision.Provisioners
	secrets      secretsOf
	tokens       *tokensOf
	hosts        *hostsOf
	catalog      provision.TypesOf
	args         []byte
	machinesDir  string
}

// arguments is the fake's entry arguments: where machines are kept.
type arguments struct {
	Dir     string `cbor:"dir"`
	Address string `cbor:"address"`
}

// newDeployment wires the provisioners to the fake provisioner, keeping machines
// in a directory of the test's.
func newDeployment(t *testing.T, opened *store.Store) *deployment {
	t.Helper()

	machinesDir := t.TempDir()
	args, err := sdk.Marshal(arguments{Dir: machinesDir, Address: "192.0.2.10"})
	if err != nil {
		t.Fatalf("encoding the arguments: %v", err)
	}
	d := &deployment{
		secrets:     secretsOf{"lab": {"example_token": "s3cret"}},
		tokens:      &tokensOf{},
		hosts:       &hostsOf{},
		machinesDir: machinesDir,
		args:        args,
		catalog: provision.TypesOf{{
			ID:      "example/fake",
			Plugin:  "example",
			Label:   "Fake machine",
			Kinds:   []string{"machine"},
			Command: fakeBinary(t),
		}},
	}
	d.provisioners = provision.New(provision.Deps{
		Store:   opened,
		Checker: access.NewChecker(opened),
		Catalog: d.catalog,
		Runner:  provision.NewRunner(filepath.Join(t.TempDir(), "server.sock"), t.TempDir(), nil),
		Secrets: d.secrets,
		Tokens:  d.tokens,
		Hosts:   d.hosts,
	})
	return d
}

// addLab adds the fake's entry, with its arguments.
func (d *deployment) addLab(t *testing.T) provision.Entry {
	t.Helper()

	entry, err := d.provisioners.Entries().Add(t.Context(), provision.Entry{
		Name:   "lab",
		Type:   "example/fake",
		Plugin: "example",
		Args:   d.args,
		Node:   "node-1",
	})
	if err != nil {
		t.Fatalf("adding the entry: %v", err)
	}
	return entry
}

// seedOf reads a machine's cloud-init seed from where the fake keeps it.
func (d *deployment) seedOf(t *testing.T, host string) string {
	t.Helper()

	seed, err := os.ReadFile(filepath.Join(d.machinesDir, host, "user-data")) //nolint:gosec // the directory is the test's.
	if err != nil {
		t.Fatalf("reading %s's seed: %v", host, err)
	}
	return string(seed)
}

// TestCreateMachine makes a machine through the fake provisioner and checks what
// the core asked it for and what it recorded.
func TestCreateMachine(t *testing.T) {
	t.Parallel()

	opened := testStore(t)
	d := newDeployment(t, opened)
	d.addLab(t)

	var lines []string
	resource, err := d.provisioners.CreateMachine(t.Context(), provision.MachineRequest{
		Host:  "desk-1",
		Entry: "lab",
		Seed: provision.Seed{
			PublicKey:  "ssh-ed25519 AAAAtest rookery",
			TailnetKey: "tskey-test-1",
		},
		Line: func(line string) { lines = append(lines, line) },
	})
	if err != nil {
		t.Fatalf("creating the machine: %v", err)
	}
	if resource.Kind != provision.KindMachine || resource.Name != "desk-1" || resource.Address != "192.0.2.10" {
		t.Fatalf("the resource is %+v", resource)
	}
	if len(resource.State) == 0 {
		t.Fatal("the resource kept no state")
	}

	seed := d.seedOf(t, "desk-1")
	for _, want := range []string{
		"#cloud-config",
		"name: ops",
		"ssh-ed25519 AAAAtest rookery",
		"ALL=(ALL) NOPASSWD:ALL",
		"--auth-key=file:/run/rookery-tailnet-key", "content: tskey-test-1", "--hostname=desk-1",
	} {
		if !strings.Contains(seed, want) {
			t.Errorf("the seed does not have %q:\n%s", want, seed)
		}
	}

	if len(d.hosts.machines) != 1 || d.hosts.machines[0].Name != "desk-1" || d.hosts.machines[0].Provisioner != "lab" {
		t.Fatalf("the hosts were given %+v", d.hosts.machines)
	}

	// The machine is listed as the core recorded it.
	resources, err := d.provisioners.Resources().List(t.Context(), "lab", provision.KindMachine, "", 10)
	if err != nil {
		t.Fatalf("listing resources: %v", err)
	}
	if len(resources) != 1 || resources[0].ID != resource.ID {
		t.Fatalf("the resources are %+v", resources)
	}

	// A host has at most one machine.
	if _, err := d.provisioners.CreateMachine(t.Context(), provision.MachineRequest{
		Host:  "desk-1",
		Entry: "lab",
		Seed:  provision.Seed{PublicKey: "ssh-ed25519 AAAAtest rookery"},
	}); err == nil {
		t.Fatal("a second machine for one host was made")
	}
}

// TestRemoveMachine removes a machine through the entry that made it.
func TestRemoveMachine(t *testing.T) {
	t.Parallel()

	opened := testStore(t)
	d := newDeployment(t, opened)
	d.addLab(t)

	resource, err := d.provisioners.CreateMachine(t.Context(), provision.MachineRequest{
		Host:  "desk-2",
		Entry: "lab",
		Seed:  provision.Seed{PublicKey: "ssh-ed25519 AAAAtest rookery"},
	})
	if err != nil {
		t.Fatalf("creating the machine: %v", err)
	}
	if _, err := os.Stat(filepath.Join(d.machinesDir, "desk-2")); err != nil {
		t.Fatalf("the machine is not there: %v", err)
	}

	if err := d.provisioners.RemoveMachine(t.Context(), "desk-2", nil); err != nil {
		t.Fatalf("removing the machine: %v", err)
	}
	if _, err := os.Stat(filepath.Join(d.machinesDir, "desk-2")); !os.IsNotExist(err) {
		t.Fatalf("the machine is still there: %v", err)
	}
	if _, err := d.provisioners.Resources().ByID(t.Context(), resource.ID); !errors.Is(err, access.ErrNotFound) {
		t.Fatalf("the resource is still recorded: %v", err)
	}
}

// TestProvisionerOutputIsALine checks that a provisioner's output reaches the
// sink the call was given.
func TestProvisionerOutputIsALine(t *testing.T) {
	t.Parallel()

	opened := testStore(t)
	d := newDeployment(t, opened)
	d.addLab(t)

	if _, err := d.provisioners.CreateMachine(t.Context(), provision.MachineRequest{
		Host:  "desk-3",
		Entry: "lab",
		Seed:  provision.Seed{PublicKey: "ssh-ed25519 AAAAtest rookery"},
	}); err != nil {
		t.Fatalf("creating the machine: %v", err)
	}

	lines, err := d.runAction(t, "seed", mustMarshal(t, map[string]string{"name": "desk-3"}))
	if err != nil {
		t.Fatalf("running the action: %v", err)
	}
	if len(lines) == 0 || lines[0] != "#cloud-config" {
		t.Fatalf("the seed came back as %v", lines)
	}
}

// runAction runs an action and collects its lines.
func (d *deployment) runAction(t *testing.T, action string, args []byte) ([]string, error) {
	t.Helper()

	lines := []string{}
	err := d.provisioners.RunAction(t.Context(), "lab", action, args, func(line string) {
		lines = append(lines, line)
	})
	return lines, err
}

func mustMarshal(t *testing.T, value any) []byte {
	t.Helper()

	encoded, err := sdk.Marshal(value)
	if err != nil {
		t.Fatalf("encoding: %v", err)
	}
	return encoded
}
