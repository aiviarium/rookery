package provisioner

import (
	"context"
	"crypto/tls"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	incus "github.com/lxc/incus/v6/client"

	"github.com/aiviarium/rookery/pkg/sdk"
)

// The tests run the provisioner against a fake of the Incus REST API: recorded
// responses in testdata, served over TLS on httptest, reached through an
// HTTP transport that ignores the address the client asks for. The client is the
// real github.com/lxc/incus/v6/client, so what is tested is the whole path from
// the provisioner through the Incus client to what it sends.

// recorded is one request the provisioner made.
type recorded struct {
	Method string
	Path   string
	Query  string
	Body   []byte
}

// fakeEngine is the Incus API, recorded.
type fakeEngine struct {
	server *httptest.Server

	mu       sync.Mutex
	requests []recorded
}

// newFakeEngine serves testdata's responses.
func newFakeEngine(t *testing.T) *fakeEngine {
	t.Helper()

	fake := &fakeEngine{}
	mux := http.NewServeMux()
	record := func(name string) http.HandlerFunc {
		return func(w http.ResponseWriter, r *http.Request) {
			body, _ := io.ReadAll(r.Body)
			fake.mu.Lock()
			fake.requests = append(fake.requests, recorded{
				Method: r.Method, Path: r.URL.Path, Query: r.URL.RawQuery, Body: body,
			})
			fake.mu.Unlock()
			serveRecorded(t, w, name)
		}
	}
	mux.HandleFunc("GET /1.0", record("server.json"))
	mux.HandleFunc("GET /1.0/events", func(w http.ResponseWriter, _ *http.Request) {
		http.Error(w, "no events here", http.StatusNotFound)
	})
	mux.HandleFunc("GET /1.0/instances", record("instances.json"))
	mux.HandleFunc("GET /1.0/virtual-machines", record("instances.json"))
	mux.HandleFunc("POST /1.0/instances", record("operation.json"))
	mux.HandleFunc("POST /1.0/virtual-machines", record("operation.json"))
	mux.HandleFunc("GET /1.0/instances/{name}", record("instance.json"))
	mux.HandleFunc("GET /1.0/virtual-machines/{name}", record("instance.json"))
	mux.HandleFunc("GET /1.0/instances/{name}/state", record("instance-state.json"))
	mux.HandleFunc("GET /1.0/virtual-machines/{name}/state", record("instance-state.json"))
	mux.HandleFunc("PUT /1.0/instances/{name}/state", record("operation.json"))
	mux.HandleFunc("PUT /1.0/virtual-machines/{name}/state", record("operation.json"))
	mux.HandleFunc("DELETE /1.0/instances/{name}", record("operation.json"))
	mux.HandleFunc("DELETE /1.0/virtual-machines/{name}", record("operation.json"))
	mux.HandleFunc("GET /1.0/images/aliases", record("aliases.json"))
	mux.HandleFunc("GET /1.0/operations/{id}", record("wait.json"))
	mux.HandleFunc("GET /1.0/operations/{id}/wait", record("wait.json"))
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		fake.mu.Lock()
		fake.requests = append(fake.requests, recorded{Method: r.Method, Path: r.URL.Path, Query: r.URL.RawQuery})
		fake.mu.Unlock()
		http.Error(w, fmt.Sprintf("the fake has no %s %s", r.Method, r.URL.Path), http.StatusNotFound)
	})

	fake.server = httptest.NewTLSServer(mux)
	t.Cleanup(fake.server.Close)
	return fake
}

// serveRecorded writes one testdata file as the response.
func serveRecorded(t *testing.T, w http.ResponseWriter, name string) {
	t.Helper()

	body, err := os.ReadFile(filepath.Join("testdata", filepath.Base(name))) //nolint:gosec // the test names the file.
	if err != nil {
		t.Errorf("reading %s: %v", name, err)
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	_, _ = w.Write(body)
}

// httpClient reaches the fake whatever address the Incus client asks for.
func (f *fakeEngine) httpClient() *http.Client {
	address := f.server.Listener.Addr().String()
	return &http.Client{Transport: &http.Transport{
		//nolint:gosec // the fake's certificate is its own.
		TLSClientConfig: &tls.Config{InsecureSkipVerify: true},
		DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
			var dialer net.Dialer
			return dialer.DialContext(ctx, "tcp", address)
		},
	}}
}

// connect returns a client for the fake.
func (f *fakeEngine) connect(context.Context) (incus.InstanceServer, error) {
	return incus.ConnectIncusHTTP(&incus.ConnectionArgs{SkipGetEvents: true}, f.httpClient())
}

// fakeProvisioner is a provisioner whose engine is the fake.
func fakeProvisioner(t *testing.T, instanceType string) (*Provisioner, *fakeEngine) {
	t.Helper()

	fake := newFakeEngine(t)
	provisioner := New(sdk.Config{}, instanceType)
	provisioner.open = func(ctx context.Context, _ sdk.Config, _ sdk.Engine, _ arguments) (incus.InstanceServer, error) {
		return fake.connect(ctx)
	}
	return provisioner, fake
}

// last is the last request whose method and path match, or a failure.
func (f *fakeEngine) last(t *testing.T, method, path string) recorded {
	t.Helper()

	f.mu.Lock()
	defer f.mu.Unlock()
	for index := len(f.requests) - 1; index >= 0; index-- {
		if f.requests[index].Method == method && f.requests[index].Path == path {
			return f.requests[index]
		}
	}
	t.Fatalf("there was no %s %s in %+v", method, path, f.requests)
	return recorded{}
}

// TestDescribe checks what the two provisioners say about themselves.
func TestDescribe(t *testing.T) {
	t.Parallel()

	for _, instanceType := range []string{TypeVM, TypeContainer} {
		description, err := New(sdk.Config{}, instanceType).Describe(t.Context())
		if err != nil {
			t.Fatalf("%s: describing: %v", instanceType, err)
		}
		if description.Type != instanceType {
			t.Errorf("%s is described as %q", instanceType, description.Type)
		}
		if len(description.Kinds) != 1 || !description.Kinds[0].Machine || description.Kinds[0].ID != KindMachine {
			t.Errorf("%s's kinds are %+v", instanceType, description.Kinds)
		}
		if len(description.Sizes) != 3 || len(description.Images) != 2 {
			t.Errorf("%s offers %d sizes and %d images", instanceType, len(description.Sizes), len(description.Images))
		}
		if len(description.Secrets) != 2 || description.Secrets[0] != SecretCert {
			t.Errorf("%s's secrets are %v", instanceType, description.Secrets)
		}
		if !description.Node || !description.Local {
			t.Errorf("%s does not say where it runs", instanceType)
		}
		var actions []string
		for _, action := range description.Actions {
			actions = append(actions, action.Name)
		}
		if strings.Join(actions, ",") != "images,instances,delete" {
			t.Errorf("%s's actions are %v", instanceType, actions)
		}
	}
}

// TestCreateMachine makes a container and checks what the provisioner sent.
func TestCreateMachine(t *testing.T) {
	t.Parallel()

	provisioner, fake := fakeProvisioner(t, TypeContainer)
	seed := "#cloud-config\nhostname: desk-1\nusers:\n  - name: ops\n"
	instance, err := provisioner.Create(t.Context(), sdk.CreateRequest{
		Entry: "lab-incus/vm",
		Kind:  KindMachine,
		Name:  "desk-1",
		Seed:  seed,
		Args:  mustMarshal(t, map[string]any{"size": "medium", "project": "lab"}),
	})
	if err != nil {
		t.Fatalf("creating: %v", err)
	}
	if instance.Address != "10.42.7.101" {
		t.Errorf("the address is %q", instance.Address)
	}
	if instance.Kind != KindMachine || instance.Name != "desk-1" || instance.ID != "desk-1" {
		t.Errorf("the instance is %+v", instance)
	}

	created := fake.last(t, "POST", "/1.0/instances")
	if !strings.Contains(created.Query, "project=lab") {
		t.Errorf("the create's query is %q", created.Query)
	}
	var post map[string]any
	if err := json.Unmarshal(created.Body, &post); err != nil {
		t.Fatalf("the create's body is not JSON: %v\n%s", err, created.Body)
	}
	if post["name"] != "desk-1" || post["type"] != "container" {
		t.Errorf("the create is %v", post)
	}
	config, _ := post["config"].(map[string]any)
	if config["cloud-init.user-data"] != seed {
		t.Errorf("the seed is %v", config["cloud-init.user-data"])
	}
	if config["limits.cpu"] != "4" || config["limits.memory"] != "8192MB" {
		t.Errorf("the sizes are %v", config)
	}
	if config[LabelEntry] != "lab-incus/vm" || config[LabelType] != TypeContainer || config[LabelName] != "desk-1" {
		t.Errorf("the labels are %v", config)
	}
	devices, _ := post["devices"].(map[string]any)
	root, _ := devices["root"].(map[string]any)
	if root["pool"] != "default" || root["size"] != "50GB" || root["path"] != "/" {
		t.Errorf("the root device is %v", root)
	}
	profiles, _ := post["profiles"].([]any)
	if len(profiles) != 1 || profiles[0] != "default" {
		t.Errorf("the profiles are %v", post["profiles"])
	}
	source, _ := post["source"].(map[string]any)
	if source["type"] != "image" || source["alias"] != "ubuntu/24.04" || source["protocol"] != "simplestreams" {
		t.Errorf("the source is %v", source)
	}

	// The machine was started after it was made.
	started := fake.last(t, "PUT", "/1.0/instances/desk-1/state")
	var state map[string]any
	if err := json.Unmarshal(started.Body, &state); err != nil {
		t.Fatalf("the state change is not JSON: %v", err)
	}
	if state["action"] != "start" {
		t.Errorf("the machine was told to %v", state["action"])
	}
}

// TestCreateVM checks that a VM is made the way Incus makes them.
func TestCreateVM(t *testing.T) {
	t.Parallel()

	provisioner, fake := fakeProvisioner(t, TypeVM)
	_, err := provisioner.Create(t.Context(), sdk.CreateRequest{
		Entry: "lab-incus/vm-cont",
		Kind:  KindMachine,
		Name:  "srv-1",
		Seed:  "#cloud-config\n",
	})
	if err != nil {
		t.Fatalf("creating: %v", err)
	}
	created := fake.last(t, "POST", "/1.0/instances")
	var post map[string]any
	if err := json.Unmarshal(created.Body, &post); err != nil {
		t.Fatalf("the create's body is not JSON: %v", err)
	}
	if post["type"] != "virtual-machine" {
		t.Errorf("the create is %v", post)
	}
	fake.last(t, "PUT", "/1.0/instances/srv-1/state")
}

// TestLifecycle checks get, stop, start and remove.
func TestLifecycle(t *testing.T) {
	t.Parallel()

	provisioner, fake := fakeProvisioner(t, TypeContainer)
	instance := sdk.Instance{
		Kind:  KindMachine,
		Name:  "desk-1",
		ID:    "desk-1",
		State: mustMarshal(t, machineState{Entry: "lab-incus/vm", Type: TypeContainer, Name: "desk-1"}),
	}

	got, err := provisioner.Get(t.Context(), instance)
	if err != nil {
		t.Fatalf("reading: %v", err)
	}
	if got.Address != "10.42.7.101" {
		t.Errorf("the address is %q", got.Address)
	}

	if err := provisioner.Stop(t.Context(), instance); err != nil {
		t.Fatalf("stopping: %v", err)
	}
	if body := actionOf(t, fake.last(t, "PUT", "/1.0/instances/desk-1/state")); body != "stop" {
		t.Errorf("the machine was told to %s", body)
	}

	if _, err := provisioner.Start(t.Context(), instance); err != nil {
		t.Fatalf("starting: %v", err)
	}
	if body := actionOf(t, fake.last(t, "PUT", "/1.0/instances/desk-1/state")); body != "start" {
		t.Errorf("the machine was told to %s", body)
	}

	if err := provisioner.Remove(t.Context(), instance); err != nil {
		t.Fatalf("removing: %v", err)
	}
	fake.last(t, "DELETE", "/1.0/instances/desk-1")
}

// TestList checks that a listing is one entry's machines of one type.
func TestList(t *testing.T) {
	t.Parallel()

	provisioner, fake := fakeProvisioner(t, TypeContainer)
	resources, err := provisioner.List(t.Context(), sdk.ListRequest{Kind: KindMachine, Entry: "lab-incus/vm"})
	if err != nil {
		t.Fatalf("listing: %v", err)
	}
	if len(resources) != 1 {
		t.Fatalf("the machines are %+v", resources)
	}
	resource := resources[0]
	if resource.Name != "desk-1" || resource.Address != "10.42.7.101" {
		t.Errorf("the machine is %+v", resource)
	}
	if resource.Labels[LabelEntry] != "lab-incus/vm" || resource.Labels[LabelType] != TypeContainer {
		t.Errorf("its labels are %v", resource.Labels)
	}
	var state machineState
	if err := sdk.Unmarshal(resource.State, &state); err != nil {
		t.Fatalf("its state is not a machine's: %v", err)
	}
	if state.Name != "desk-1" {
		t.Errorf("its state is %+v", state)
	}
	fake.last(t, "GET", "/1.0/instances")

	// A VM provisioner sees no containers, even ones carrying its entry's name.
	vm, _ := fakeProvisioner(t, TypeVM)
	machines, err := vm.List(t.Context(), sdk.ListRequest{Entry: "lab-incus/vm"})
	if err != nil {
		t.Fatalf("listing VMs: %v", err)
	}
	if len(machines) != 0 {
		t.Errorf("a container is a VM: %+v", machines)
	}
}

// TestStatus checks the version the server answers with.
func TestStatus(t *testing.T) {
	t.Parallel()

	provisioner, _ := fakeProvisioner(t, TypeContainer)
	status, err := provisioner.Status(t.Context(), sdk.Engine{}, nil)
	if err != nil {
		t.Fatalf("reading the status: %v", err)
	}
	if !status.Up || status.Message != "Incus 6.23" {
		t.Errorf("the status is %+v", status)
	}

	// A node's socket is the server's, and a process without one cannot reach
	// it.
	if _, err := openEngine(t.Context(), sdk.Config{}, sdk.Engine{Node: "lab"}, arguments{}); err == nil || !strings.Contains(err.Error(), sdk.SocketEnv) {
		t.Errorf("a node with no server gave %v", err)
	}
	// A remote server needs the client certificate.
	if _, err := openEngine(t.Context(), sdk.Config{}, sdk.Engine{Remote: "incus.example.com"}, arguments{}); err == nil || !strings.Contains(err.Error(), SecretCert) {
		t.Errorf("a remote server with no certificate gave %v", err)
	}
}

// TestActions checks the three actions.
func TestActions(t *testing.T) {
	t.Parallel()

	provisioner, fake := fakeProvisioner(t, TypeContainer)
	lines, err := collect(func(line func(string)) error {
		return provisioner.RunAction(t.Context(), sdk.ActionRequest{Action: "images"}, line)
	})
	if err != nil {
		t.Fatalf("listing the images: %v", err)
	}
	if len(lines) != 3 || !strings.HasPrefix(lines[0], "ubuntu/24.04") {
		t.Errorf("the images are %v", lines)
	}

	lines, err = collect(func(line func(string)) error {
		return provisioner.RunAction(t.Context(), sdk.ActionRequest{Action: "instances"}, line)
	})
	if err != nil {
		t.Fatalf("listing the instances: %v", err)
	}
	if len(lines) != 2 || !strings.HasPrefix(lines[0], "desk-1") {
		t.Errorf("the instances are %v", lines)
	}

	if err := provisioner.RunAction(t.Context(), sdk.ActionRequest{Action: "delete"}, func(string) {}); err == nil {
		t.Error("a delete without a name worked")
	}
	lines, err = collect(func(line func(string)) error {
		return provisioner.RunAction(t.Context(), sdk.ActionRequest{
			Action: "delete", Args: mustMarshal(t, map[string]any{"name": "desk-1"}),
		}, line)
	})
	if err != nil {
		t.Fatalf("deleting: %v", err)
	}
	if len(lines) != 1 || lines[0] != "removed desk-1" {
		t.Errorf("the delete said %v", lines)
	}
	fake.last(t, "DELETE", "/1.0/instances/desk-1")

	if err := provisioner.RunAction(t.Context(), sdk.ActionRequest{Action: "nonsense"}, func(string) {}); err == nil {
		t.Error("an action that does not exist worked")
	}
}

// TestRefusals checks what a machine is refused before anything is made.
func TestRefusals(t *testing.T) {
	t.Parallel()

	parsed, err := parseArguments(nil)
	if err != nil {
		t.Fatalf("parsing: %v", err)
	}
	for name, request := range map[string]sdk.CreateRequest{
		"no name": {Entry: "lab-incus/vm", Kind: KindMachine, Seed: "#cloud-config\n"},
		"a name Incus cannot have": {
			Entry: "lab-incus/vm", Kind: KindMachine, Name: "9-lives", Seed: "#cloud-config\n",
		},
		"no seed": {Entry: "lab-incus/vm", Kind: KindMachine, Name: "desk-1"},
	} {
		if _, err := createMachine(t.Context(), nil, TypeContainer, parsed, request); err == nil {
			t.Errorf("%s worked", name)
		}
	}
	if _, err := (arguments{Size: "huge"}).size(); err == nil {
		t.Error("a size that is not one worked")
	}
}

// TestParseArguments checks an entry's arguments and their defaults.
func TestParseArguments(t *testing.T) {
	t.Parallel()

	parsed, err := parseArguments(nil)
	if err != nil {
		t.Fatalf("parsing: %v", err)
	}
	if parsed.Image != DefaultImage || parsed.Size != "small" || parsed.Profile != "default" || parsed.Pool != "default" {
		t.Errorf("the defaults are %+v", parsed)
	}
	if parsed.socket() != DefaultSocket {
		t.Errorf("the socket is %q", parsed.socket())
	}

	tests := map[string][3]string{
		"images:debian/12":                {"https://images.linuxcontainers.org", "debian/12", "simplestreams"},
		"debian/12":                       {"https://images.linuxcontainers.org", "debian/12", "simplestreams"},
		"images.example.com:ubuntu/24.04": {"https://images.example.com", "ubuntu/24.04", "simplestreams"},
		"localalias":                      {"https://images.linuxcontainers.org", "localalias", "simplestreams"},
	}
	for image, want := range tests {
		server, alias, protocol := (arguments{Image: image}).image()
		if server != want[0] || alias != want[1] || protocol != want[2] {
			t.Errorf("%q is %q %q %q", image, server, alias, protocol)
		}
	}
}

// TestNames checks the names Incus accepts.
func TestNames(t *testing.T) {
	t.Parallel()

	for name, want := range map[string]bool{
		"desk-1":                true,
		"desk":                  true,
		"9-lives":               false,
		"desk_1":                false,
		"desk/1":                false,
		"":                      false,
		"a b":                   false,
		strings.Repeat("a", 64): false,
	} {
		if got := validName(name); got != want {
			t.Errorf("validName(%q) = %v", name, got)
		}
	}
}

// actionOf is a state change's action.
func actionOf(t *testing.T, request recorded) string {
	t.Helper()

	var state map[string]any
	if err := json.Unmarshal(request.Body, &state); err != nil {
		t.Fatalf("the state change is not JSON: %v", err)
	}
	action, _ := state["action"].(string)
	return action
}

// collect runs a stream of lines into a slice.
func collect(run func(line func(string)) error) ([]string, error) {
	lines := []string{}
	err := run(func(line string) { lines = append(lines, line) })
	return lines, err
}

// mustMarshal encodes a value, failing the test if it cannot.
func mustMarshal(t *testing.T, value any) []byte {
	t.Helper()

	encoded, err := sdk.Marshal(value)
	if err != nil {
		t.Fatalf("encoding: %v", err)
	}
	return encoded
}
