package apply_test

import (
	"context"
	"encoding/json"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/aiviarium/rookery/internal/apply"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/profile"
)

// sink collects the lines of a run, as a job's log does.
type sink struct {
	mutex sync.Mutex
	lines []string
	// err, when set, is what the sink answers with.
	err error
}

func (s *sink) Line(_ context.Context, line string) error {
	s.mutex.Lock()
	defer s.mutex.Unlock()
	s.lines = append(s.lines, line)
	return s.err
}

func (s *sink) all() []string {
	s.mutex.Lock()
	defer s.mutex.Unlock()
	return slices.Clone(s.lines)
}

// constSecret is what the fake ansible-playbook prints and the run must mask.
const constSecret = "s3cr3t-token-value"

// request is one apply of a managed host with one feature.
type fixture struct {
	request apply.Request
	runner  apply.Runner
	plugin  string // the plugin folder the feature's role is in
	sink    *sink
	fake    *fake
}

func newFixture(t *testing.T) *fixture {
	t.Helper()

	fake := newFake(t, "ROOKERY_FAKE_ANSIBLE_SECRET="+constSecret)
	plugin := pluginFolder(t, "example", "hello")
	featureVars, err := cborx.Marshal(map[string]any{"greeting": "hello"})
	if err != nil {
		t.Fatalf("encoding the feature's variables: %v", err)
	}
	profileVars, err := cborx.Marshal(map[string]any{"base_ssh_permit_root_login": "no"})
	if err != nil {
		t.Fatalf("encoding the profile's variables: %v", err)
	}
	log := &sink{}

	return &fixture{
		plugin: plugin,
		sink:   log,
		fake:   fake,
		runner: apply.Runner{
			StateDir: t.TempDir(),
			Playbook: fake.Path,
			Rookery:  "/usr/bin/rookery",
		},
		request: apply.Request{
			JobID: "01j0job",
			Host:  apply.Host{Name: "desk-1"},
			Profile: profile.Profile{
				Type:     profile.TypeManaged,
				Address:  "192.0.2.10",
				SSHUser:  "ops",
				Operator: "ops",
				Tailnet:  true,
				Vars:     profileVars,
				Features: []profile.Feature{
					{Plugin: "example", Feature: "hello", Vars: featureVars},
				},
			},
			Plugins: map[string]string{"example": "0.1.0"},
			Rookery: "9.9.9",
			Key: apply.KeyMaterial{
				PrivateKey: testKey(t),
				KnownHost:  []byte("desk-1 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIexample\n"),
			},
			Features: []apply.Feature{{
				Plugin:  "example",
				Feature: "hello",
				Dir:     plugin,
				Path:    "features/hello",
				Vars:    featureVars,
				Secrets: map[string]string{"example_token": constSecret},
			}},
			Secrets: map[string]string{"base_token": constSecret},
			Registries: []apply.Registry{{
				Address: "registry.example", Username: "someone", Password: constSecret,
			}},
			AuthKey:  "tskey-auth-example",
			Binaries: apply.Binaries{Rookeryd: testBinary(t), Box: testBinary(t)},
			Actions: []apply.Action{{
				Name: "status", Label: "Status", User: "operator", Run: "/usr/local/bin/status",
				Args: []apply.ActionArg{{Name: "verbose", Label: "Verbose"}},
			}},
			Sink: log,
		},
	}
}

func TestApplyWritesTheWorkDirectory(t *testing.T) {
	t.Parallel()

	f := newFixture(t)
	result, err := f.runner.Apply(context.Background(), f.request)
	if err != nil {
		t.Fatalf("Apply() = %v", err)
	}
	seen := f.fake.seen(t)

	// The command line is the plan's: inventory, site.yml, the variables.
	wantArgs := []string{
		"-i", filepath.Join(result.WorkDir, "inventory.yml"),
		filepath.Join(result.WorkDir, "site.yml"),
		"-e", "@" + filepath.Join(result.WorkDir, "vars.json"),
	}
	if !slices.Equal(seen.Args, wantArgs) {
		t.Errorf("ansible-playbook ran as %q, want %q", seen.Args, wantArgs)
	}
	if seen.Dir != result.WorkDir {
		t.Errorf("ran in %q, want %q", seen.Dir, result.WorkDir)
	}

	// The work directory is gone afterwards.
	if _, err := os.Stat(result.WorkDir); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("the work directory is still there: %v", err)
	}

	// The secrets' files are for the server's user only; the playbook is not.
	for _, name := range []string{"key", "known_hosts", "inventory.yml", "vars.json"} {
		if got := seen.Modes[name]; got != "0600" {
			t.Errorf("%s has mode %q, want 0600", name, got)
		}
	}
	for _, name := range []string{"site.yml", "ansible.cfg"} {
		if got := seen.Modes[name]; got != "0644" {
			t.Errorf("%s has mode %q, want 0644", name, got)
		}
	}

	// The embedded roles are extracted and the feature is linked in.
	extracted, ok := seen.Roles["base"]
	if !ok {
		t.Fatalf("the base role is not in %v", seen.Roles)
	}
	if !strings.HasSuffix(extracted, filepath.Join("roles", "base")) {
		t.Errorf("the base role resolves to %q", extracted)
	}
	linked, ok := seen.Roles["example__hello"]
	if !ok {
		t.Fatalf("the feature's role is not in %v", seen.Roles)
	}
	if linked != filepath.Join(f.plugin, "features", "hello") {
		t.Errorf("the feature's role resolves to %q, want the plugin's folder", linked)
	}

	// The playbook runs the base, then the features in profile order.
	playbook := seen.Files["site.yml"]
	if !strings.Contains(playbook, "- base") || !strings.Contains(playbook, "- example__hello") {
		t.Errorf("site.yml does not name its roles:\n%s", playbook)
	}
	if strings.Index(playbook, "base") > strings.Index(playbook, "example__hello") {
		t.Errorf("site.yml runs the feature before the base:\n%s", playbook)
	}

	// The inventory is the profile's address, its user and its port.
	inventory := seen.Files["inventory.yml"]
	for _, want := range []string{
		`"desk-1":`, `ansible_host: "192.0.2.10"`, "ansible_port: 22", `ansible_user: "ops"`,
	} {
		if !strings.Contains(inventory, want) {
			t.Errorf("inventory.yml has no %q:\n%s", want, inventory)
		}
	}
	if strings.Contains(inventory, "ProxyCommand") {
		t.Errorf("inventory.yml proxies a host without a tailnet address:\n%s", inventory)
	}

	// vars.json carries what the roles read, and the digest covers it.
	var vars map[string]any
	if err := json.Unmarshal([]byte(seen.Files["vars.json"]), &vars); err != nil {
		t.Fatalf("vars.json is not JSON: %v", err)
	}
	for _, name := range []string{
		"base_operator", "base_operator_key", "base_tailnet_auth_key", "base_rookeryd_path",
		"base_box_path", "base_registries", "base_secrets", "base_actions",
		"base_ssh_permit_root_login", "example__hello",
	} {
		if _, ok := vars[name]; !ok {
			t.Errorf("vars.json has no %s: %v", name, keys(vars))
		}
	}
	if got := vars["base_tailnet_auth_key"]; got != "tskey-auth-example" {
		t.Errorf("base_tailnet_auth_key = %v", got)
	}
	if key, _ := vars["base_operator_key"].(string); !strings.HasPrefix(key, "ssh-ed25519 ") {
		t.Errorf("base_operator_key = %q, want the ring's public key", key)
	}
	feature, ok := vars["example__hello"].(map[string]any)
	if !ok {
		t.Fatalf("example__hello = %v, want the feature's variables", vars["example__hello"])
	}
	if got, _ := feature["vars"].(map[string]any); got["greeting"] != "hello" {
		t.Errorf("the feature's variables = %v", feature["vars"])
	}
	if got, _ := feature["secrets"].(map[string]any); got["example_token"] != constSecret {
		t.Errorf("the feature's secrets = %v", feature["secrets"])
	}
	if result.Digest == "" {
		t.Error("Apply() returned no digest")
	}

	// The child's environment carries nothing of the server's but what Ansible
	// needs.
	if _, ok := seen.Env["ANSIBLE_CONFIG"]; !ok {
		t.Error("the run has no ANSIBLE_CONFIG")
	}
	if got := seen.Env["HOME"]; got != filepath.Join(result.WorkDir, "home") {
		t.Errorf("HOME = %q, want the work directory's", got)
	}
}

// keys is a map's names, for a failure message.
func keys(m map[string]any) []string {
	names := make([]string, 0, len(m))
	for name := range m {
		names = append(names, name)
	}
	return names
}

func TestApplyReachesATailnetHostThroughTheServer(t *testing.T) {
	t.Parallel()

	f := newFixture(t)
	f.request.Host.TailnetAddress = "100.64.0.7"
	result, err := f.runner.Apply(context.Background(), f.request)
	if err != nil {
		t.Fatalf("Apply() = %v", err)
	}
	seen := f.fake.seen(t)

	inventory := seen.Files["inventory.yml"]
	for _, want := range []string{
		`ansible_host: "100.64.0.7"`,
		`-o ProxyCommand=/usr/bin/rookery server dial %h %p`,
	} {
		if !strings.Contains(inventory, want) {
			t.Errorf("inventory.yml has no %q:\n%s", want, inventory)
		}
	}
	_ = result
}

func TestApplyMasksSecrets(t *testing.T) {
	t.Parallel()

	f := newFixture(t)
	if _, err := f.runner.Apply(context.Background(), f.request); err != nil {
		t.Fatalf("Apply() = %v", err)
	}
	lines := f.sink.all()
	if len(lines) == 0 {
		t.Fatal("the sink got no lines")
	}
	for _, line := range lines {
		if strings.Contains(line, constSecret) {
			t.Errorf("a line of the log holds a secret: %q", line)
		}
	}
	if !slices.ContainsFunc(lines, func(line string) bool { return strings.Contains(line, "***") }) {
		t.Errorf("no line was masked: %q", lines)
	}
}

func TestApplyFails(t *testing.T) {
	t.Parallel()

	f := newFixture(t)
	f.fake = newFake(t, "ROOKERY_FAKE_ANSIBLE_MODE=fail")
	f.runner.Playbook = f.fake.Path

	_, err := f.runner.Apply(context.Background(), f.request)
	if err == nil {
		t.Fatal("Apply() succeeded with a failing playbook")
	}
	if !strings.Contains(err.Error(), "ansible-playbook failed") {
		t.Errorf("error = %v, want the playbook's failure", err)
	}
}

func TestApplyCancelsTheProcessGroup(t *testing.T) {
	t.Parallel()

	f := newFixture(t)
	f.fake = newFake(t, "ROOKERY_FAKE_ANSIBLE_MODE=sleep")
	f.runner.Playbook = f.fake.Path

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { _, err := f.runner.Apply(ctx, f.request); done <- err }()

	// Cancel once the run is under way: the fake writes its record as it starts.
	deadline := time.Now().Add(10 * time.Second)
	for {
		if _, ok := f.fake.maybeSeen(t); ok {
			break
		}
		if time.Now().After(deadline) {
			cancel()
			t.Fatal("the fake ansible-playbook never started")
		}
		time.Sleep(10 * time.Millisecond)
	}
	cancel()

	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Errorf("Apply() = %v, want a cancelled run", err)
		}
	case <-time.After(20 * time.Second):
		t.Fatal("the run outlived the cancelled context: the process group was not killed")
	}
}

func TestApplyFindsPlaybookOnPath(t *testing.T) {
	// The PATH is the process's, so this test does not run in parallel.
	f := newFixture(t)
	// A second fake keeps the first's record out of this one's run.
	fake := newFake(t)
	t.Setenv("PATH", filepath.Dir(fake.Path)+string(os.PathListSeparator)+os.Getenv("PATH"))
	f.runner.Playbook = ""

	if _, err := f.runner.Apply(context.Background(), f.request); err != nil {
		t.Fatalf("Apply() = %v", err)
	}
	if _, ok := fake.maybeSeen(t); !ok {
		t.Error("the ansible-playbook first on PATH did not run")
	}
}

func TestApplyRefuses(t *testing.T) {
	t.Parallel()

	invalidVars, err := cborx.Marshal(map[string]any{"base_tailnet_auth_key": "mine"})
	if err != nil {
		t.Fatalf("encoding variables: %v", err)
	}
	notAVar, err := cborx.Marshal(map[string]any{"not_a_base_var": "x"})
	if err != nil {
		t.Fatalf("encoding variables: %v", err)
	}

	cases := []struct {
		name   string
		change func(*apply.Runner, *apply.Request)
	}{
		{"no state directory", func(r *apply.Runner, _ *apply.Request) { r.StateDir = "" }},
		{"job id with a slash", func(_ *apply.Runner, req *apply.Request) { req.JobID = "../escape" }},
		{"no host name", func(_ *apply.Runner, req *apply.Request) { req.Host.Name = "" }},
		{"no address", func(_ *apply.Runner, req *apply.Request) { req.Profile.Address = "" }},
		{"no key", func(_ *apply.Runner, req *apply.Request) { req.Key.PrivateKey = nil }},
		{"no known host", func(_ *apply.Runner, req *apply.Request) { req.Key.KnownHost = nil }},
		{"no sink", func(_ *apply.Runner, req *apply.Request) { req.Sink = nil }},
		{"no binaries", func(_ *apply.Runner, req *apply.Request) { req.Binaries.Box = "" }},
		{"a binary that is not there", func(_ *apply.Runner, req *apply.Request) {
			req.Binaries.Rookeryd = filepath.Join(t.TempDir(), "missing")
		}},
		{"a feature folder that is not there", func(_ *apply.Runner, req *apply.Request) {
			req.Features[0].Dir = filepath.Join(t.TempDir(), "missing")
		}},
		{"a reserved variable", func(_ *apply.Runner, req *apply.Request) { req.Profile.Vars = invalidVars }},
		{"a variable that is not the base role's", func(_ *apply.Runner, req *apply.Request) {
			req.Profile.Vars = notAVar
		}},
		{"a key that is not a key", func(_ *apply.Runner, req *apply.Request) { req.Key.PrivateKey = []byte("nope") }},
	}

	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			f := newFixture(t)
			c.change(&f.runner, &f.request)

			if _, err := f.runner.Apply(context.Background(), f.request); !errors.Is(err, apply.ErrInvalid) {
				t.Fatalf("Apply() = %v, want a refusal", err)
			}
			if _, ok := f.fake.maybeSeen(t); ok {
				t.Error("the playbook ran for a request that was refused")
			}
		})
	}
}

func TestApplyStopsOnASinkError(t *testing.T) {
	t.Parallel()

	f := newFixture(t)
	f.sink.err = errors.New("the job's log is gone")

	_, err := f.runner.Apply(context.Background(), f.request)
	if err == nil || !strings.Contains(err.Error(), "the job's log is gone") {
		t.Fatalf("Apply() = %v, want the sink's error", err)
	}
}

func TestMasker(t *testing.T) {
	t.Parallel()

	masker := apply.NewMasker("hunter2", "", "hunter2", "abc")
	if got := masker.Len(); got != 2 {
		t.Errorf("Len() = %d, want 2: duplicates and empty values are dropped", got)
	}
	if got, want := masker.Mask("pass hunter2 and abc"), "pass *** and ***"; got != want {
		t.Errorf("Mask() = %q, want %q", got, want)
	}
	if got, want := masker.Mask("nothing here"), "nothing here"; got != want {
		t.Errorf("Mask() = %q, want %q", got, want)
	}

	// The longer value goes first, so a secret that contains another is masked
	// whole rather than in pieces.
	overlapping := apply.NewMasker("abcdef", "abc")
	if got, want := overlapping.Mask("abcdef and abc"), "*** and ***"; got != want {
		t.Errorf("Mask() = %q, want %q", got, want)
	}
}

func TestApplyMissingBaseRole(t *testing.T) {
	t.Parallel()

	f := newFixture(t)
	f.runner.Content = emptyContent{}
	_, err := f.runner.Apply(context.Background(), f.request)
	if err == nil {
		t.Fatal("Apply() ran without a base role")
	}
	if !strings.Contains(err.Error(), "no base role") {
		t.Errorf("error = %v, want the missing role", err)
	}
}

// emptyContent is an Ansible bundle with nothing in it.
type emptyContent struct{}

func (emptyContent) Open(string) (fs.File, error) { return nil, fs.ErrNotExist }
