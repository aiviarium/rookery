package sdk_test

import (
	"context"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"connectrpc.com/connect"

	"github.com/aiviarium/rookery/pkg/sdk"
)

// fake is a provisioner that keeps nothing and answers from its arguments.
type fake struct {
	description string
}

func (f fake) Describe(context.Context) (sdk.Description, error) {
	return sdk.Description{
		Type:  "test/fake",
		Label: f.description,
		Kinds: []sdk.Kind{{ID: "machine", Label: "Machine", Machine: true}},
		Sizes: []sdk.Size{{ID: "small", Label: "Small", CPUs: 2, MemoryMB: 2048, DiskGB: 20}},
		Actions: []sdk.Action{
			{Name: "images", Label: "Images"},
			{Name: "prune", Label: "Prune", Destructive: true},
		},
		Node:  true,
		Local: true,
	}, nil
}

func (f fake) Status(_ context.Context, _ sdk.Engine, args []byte) (sdk.Status, error) {
	if len(args) == 0 {
		return sdk.Status{Up: false, Message: "no arguments"}, nil
	}
	return sdk.Status{Up: true, Message: "fake 1.0"}, nil
}

func (f fake) Create(_ context.Context, request sdk.CreateRequest) (sdk.Instance, error) {
	if request.Name == "" {
		return sdk.Instance{}, fmt.Errorf("sdk_test: a machine needs a name")
	}
	state, err := sdk.Marshal(map[string]string{"id": "fake-" + request.Name})
	if err != nil {
		return sdk.Instance{}, err
	}
	return sdk.Instance{Kind: request.Kind, Name: request.Name, ID: "fake-" + request.Name, State: state, Address: "192.0.2.10"}, nil
}

func (f fake) Get(_ context.Context, instance sdk.Instance) (sdk.Instance, error) {
	instance.Address = "192.0.2.11"
	return instance, nil
}

func (f fake) Start(_ context.Context, instance sdk.Instance) (sdk.Instance, error) {
	return instance, nil
}

func (f fake) Stop(context.Context, sdk.Instance) error { return nil }

func (f fake) Remove(context.Context, sdk.Instance) error { return nil }

func (f fake) List(context.Context, sdk.ListRequest) ([]sdk.Resource, error) {
	state, err := sdk.Marshal(map[string]string{"id": "fake-one"})
	if err != nil {
		return nil, err
	}
	return []sdk.Resource{{
		Kind:    "machine",
		Name:    "one",
		ID:      "fake-one",
		Address: "192.0.2.12",
		State:   state,
		Labels:  map[string]string{"user.rookery.managed": "true"},
	}}, nil
}

func (f fake) RunAction(_ context.Context, request sdk.ActionRequest, line func(string)) error {
	if request.Action == "boom" {
		return fmt.Errorf("sdk_test: the action failed")
	}
	line("one")
	line("two")
	return nil
}

// serveFake serves a fake provisioner on a socket of its own.
func serveFake(t *testing.T, provisioner sdk.Provisioner) *sdk.ProvisionerClient {
	t.Helper()

	socket := filepath.Join(t.TempDir(), "provisioner.sock")
	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan error, 1)
	go func() { done <- sdk.ServeProvisionerOn(ctx, socket, provisioner) }()
	t.Cleanup(func() {
		cancel()
		if err := <-done; err != nil {
			t.Errorf("serving: %v", err)
		}
	})

	httpClient, base := sdk.DialSocket(socket)
	client := sdk.NewProvisionerClient(httpClient, base)

	// Wait for the socket, so the first call does not race the listen.
	deadline := time.Now().Add(5 * time.Second)
	for {
		if _, err := os.Stat(socket); err == nil {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("the provisioner's socket did not come up")
		}
		time.Sleep(5 * time.Millisecond)
	}
	return client
}

func TestServeProvisioner(t *testing.T) {
	t.Parallel()

	ctx := t.Context()
	client := serveFake(t, fake{description: "Fake machine"})

	description, err := client.Describe(ctx)
	if err != nil {
		t.Fatalf("describing: %v", err)
	}
	if description.Type != "test/fake" || len(description.Kinds) != 1 || !description.Kinds[0].Machine {
		t.Fatalf("the description is %+v", description)
	}
	if len(description.Actions) != 2 || !description.Actions[1].Destructive {
		t.Fatalf("the actions are %+v", description.Actions)
	}

	status, err := client.Status(ctx, sdk.Engine{Node: "lab"}, []byte{0xa0})
	if err != nil {
		t.Fatalf("reading the status: %v", err)
	}
	if !status.Up || status.Message != "fake 1.0" {
		t.Fatalf("the status is %+v", status)
	}

	instance, err := client.Create(ctx, sdk.CreateRequest{
		Engine: sdk.Engine{Node: "lab"},
		Entry:  "lab",
		Kind:   "machine",
		Name:   "desk-1",
		Args:   []byte{0xa0},
		Seed:   "#cloud-config\n",
	})
	if err != nil {
		t.Fatalf("creating: %v", err)
	}
	if instance.ID != "fake-desk-1" || instance.Address != "192.0.2.10" || len(instance.State) == 0 || instance.Engine.Node != "lab" {
		t.Fatalf("the machine is %+v", instance)
	}
	var state map[string]string
	if err := sdk.Unmarshal(instance.State, &state); err != nil {
		t.Fatalf("decoding the state: %v", err)
	}
	if state["id"] != "fake-desk-1" {
		t.Fatalf("the state is %v", state)
	}

	// A refusal from the implementation is an error, not a crash.
	if _, err := client.Create(ctx, sdk.CreateRequest{Kind: "machine"}); err == nil {
		t.Fatal("creating a machine with no name worked")
	}

	resources, err := client.List(ctx, sdk.ListRequest{Kind: "machine", Args: []byte{0xa0}})
	if err != nil {
		t.Fatalf("listing: %v", err)
	}
	if len(resources) != 1 || resources[0].Labels["user.rookery.managed"] != "true" {
		t.Fatalf("the resources are %+v", resources)
	}

	var lines []string
	if err := client.RunAction(ctx, sdk.ActionRequest{Action: "images"}, func(line string) { lines = append(lines, line) }); err != nil {
		t.Fatalf("running an action: %v", err)
	}
	if strings.Join(lines, ",") != "one,two" {
		t.Fatalf("the action's lines are %v", lines)
	}
	if err := client.RunAction(ctx, sdk.ActionRequest{Action: "boom"}, func(string) {}); err == nil {
		t.Fatal("a failing action did not fail")
	}
}

func TestNodeDialer(t *testing.T) {
	t.Parallel()

	ctx := t.Context()
	harness := sdk.StartHarness(t)

	// A node's Unix socket, which is what an engine on the node looks like.
	socket := filepath.Join(t.TempDir(), "engine.sock")
	var listenConfig net.ListenConfig
	listener, err := listenConfig.Listen(t.Context(), "unix", socket)
	if err != nil {
		t.Fatalf("listening: %v", err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	go func() {
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			go func() {
				defer func() { _ = conn.Close() }()
				_, _ = io.Copy(conn, conn)
			}()
		}
	}()

	dialer := harness.Client().Node("lab")
	conn, err := dialer.DialContext(ctx, "unix", socket)
	if err != nil {
		t.Fatalf("dialling the node: %v", err)
	}
	defer func() { _ = conn.Close() }()

	if _, err := conn.Write([]byte("hello")); err != nil {
		t.Fatalf("writing: %v", err)
	}
	echo := make([]byte, len("hello"))
	if _, err := io.ReadFull(conn, echo); err != nil {
		t.Fatalf("reading: %v", err)
	}
	if string(echo) != "hello" {
		t.Fatalf("the echo is %q", echo)
	}

	dials := harness.Dials()
	if len(dials) != 1 || dials[0].Node != "lab" || dials[0].UnixPath != socket {
		t.Fatalf("the harness was asked to reach %+v", dials)
	}

	// A tcp target asks for the node's loopback port. The stream's first message
	// travels asynchronously, so wait for the harness to record it.
	portConn, err := dialer.DialContext(ctx, "tcp", "example.test:2375")
	if err != nil {
		t.Fatalf("dialling a port on the node: %v", err)
	}
	defer func() { _ = portConn.Close() }()

	deadline := time.Now().Add(5 * time.Second)
	for len(harness.Dials()) < 2 && time.Now().Before(deadline) {
		time.Sleep(5 * time.Millisecond)
	}
	dials = harness.Dials()
	if len(dials) != 2 || dials[1].TCPPort != 2375 {
		t.Fatalf("the harness was asked to reach %+v", dials)
	}
}

func TestNodeDialerRefusesAnUnknownNetwork(t *testing.T) {
	t.Parallel()

	harness := sdk.StartHarness(t)
	if _, err := harness.Client().Node("lab").DialContext(t.Context(), "udp", "127.0.0.1:53"); err == nil {
		t.Fatal("a udp dial to a node worked")
	}
}

func TestServerSocketNeedsTheToken(t *testing.T) {
	t.Parallel()

	harness := sdk.StartHarness(t)
	config := harness.Config()
	config.Token = "not-the-token"
	unauthenticated, err := sdk.NewClient(config)
	if err != nil {
		t.Fatalf("connecting: %v", err)
	}

	socket := filepath.Join(t.TempDir(), "engine.sock")
	var listenConfig net.ListenConfig
	listener, err := listenConfig.Listen(t.Context(), "unix", socket)
	if err != nil {
		t.Fatalf("listening: %v", err)
	}
	t.Cleanup(func() { _ = listener.Close() })

	// The stream opens before the server answers, so the refusal arrives on the
	// first read.
	conn, err := unauthenticated.DialNode(t.Context(), sdk.Target{Node: "lab", UnixPath: socket})
	if err != nil {
		return
	}
	defer func() { _ = conn.Close() }()
	_, err = conn.Read(make([]byte, 1))
	if connect.CodeOf(err) != connect.CodeUnauthenticated && !strings.Contains(fmt.Sprint(err), "401") {
		t.Fatalf("a call with the wrong token failed with %v, want unauthenticated", err)
	}
	if len(harness.Dials()) != 0 {
		t.Fatalf("a call with the wrong token reached the node: %+v", harness.Dials())
	}
}

func TestConfigFromEnv(t *testing.T) {
	t.Setenv(sdk.SocketEnv, "/run/rookery/rookery.sock")
	t.Setenv(sdk.PluginSocketEnv, "/run/rookery/plugin.sock")
	t.Setenv(sdk.TokenEnv, "rkplug_test")

	config, err := sdk.ConfigFromEnv()
	if err != nil {
		t.Fatalf("reading the environment: %v", err)
	}
	if config.Socket != "/run/rookery/rookery.sock" || config.PluginSocket != "/run/rookery/plugin.sock" || config.Token != "rkplug_test" {
		t.Fatalf("the configuration is %+v", config)
	}

	t.Setenv(sdk.PluginSocketEnv, "")
	if _, err := sdk.ConfigFromEnv(); err == nil {
		t.Fatal("a plugin with no socket read as configured")
	}
}

// TestServeRefusesAStaleFile checks that a file that is not a socket is not
// removed to make room for one.
func TestServeRefusesAStaleFile(t *testing.T) {
	t.Parallel()

	socket := filepath.Join(t.TempDir(), "plugin.sock")
	if err := os.WriteFile(socket, []byte("not a socket"), 0o600); err != nil {
		t.Fatalf("writing: %v", err)
	}
	if err := sdk.ServeProvisionerOn(t.Context(), socket, fake{}); err == nil {
		t.Fatal("serving over a file that is not a socket worked")
	}
}
