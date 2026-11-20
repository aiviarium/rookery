package tailnet_test

import (
	"context"
	"errors"
	"net"
	"testing"

	"tailscale.com/ipn"

	"github.com/aiviarium/rookery/internal/tailnet"
)

// memoryStore is a Store in memory, which the node's state adapter is tested
// against.
type memoryStore struct {
	values map[string][]byte
	// err, when set, is what every call answers with.
	err error
}

func (m *memoryStore) Get(_ context.Context, key string) ([]byte, error) {
	if m.err != nil {
		return nil, m.err
	}
	value, ok := m.values[key]
	if !ok {
		return nil, tailnet.ErrNoState
	}
	return value, nil
}

func (m *memoryStore) Set(_ context.Context, key string, value []byte) error {
	if m.err != nil {
		return m.err
	}
	if m.values == nil {
		m.values = map[string][]byte{}
	}
	if value == nil {
		delete(m.values, key)
		return nil
	}
	m.values[key] = value
	return nil
}

func TestNodeStateIsTheIPNStateStore(t *testing.T) {
	t.Parallel()

	store := &memoryStore{}
	// A node's state store is what tsnet asks for; the adapter is unexported, so
	// this is what the package builds one with.
	node, err := tailnet.NewNode(tailnet.NodeOptions{Store: store, Dir: t.TempDir()})
	if err != nil {
		t.Fatalf("NewNode() = %v", err)
	}
	if node == nil {
		t.Fatal("NewNode() = nil")
	}
}

func TestStateRoundTrip(t *testing.T) {
	t.Parallel()

	store := &memoryStore{}
	state := tailnet.NewStateStore(store)

	// Nothing written yet is the state not existing, which is what makes tsnet
	// join the tailnet with the auth key it was given.
	_, err := state.ReadState(ipn.StateKey("_machinekey"))
	if !errors.Is(err, ipn.ErrStateNotExist) {
		t.Fatalf("ReadState() = %v, want the state not existing", err)
	}

	if err := state.WriteState(ipn.StateKey("_machinekey"), []byte("mkey:abc")); err != nil {
		t.Fatalf("WriteState() = %v", err)
	}
	value, err := state.ReadState(ipn.StateKey("_machinekey"))
	if err != nil {
		t.Fatalf("ReadState() = %v", err)
	}
	if string(value) != "mkey:abc" {
		t.Errorf("ReadState() = %q", value)
	}

	// A nil write deletes, which is how tsnet logs out.
	if err := state.WriteState(ipn.StateKey("_machinekey"), nil); err != nil {
		t.Fatalf("WriteState(nil) = %v", err)
	}
	if _, err := state.ReadState(ipn.StateKey("_machinekey")); !errors.Is(err, ipn.ErrStateNotExist) {
		t.Errorf("ReadState() after deleting = %v, want the state not existing", err)
	}
}

func TestStateReportsTheStoreError(t *testing.T) {
	t.Parallel()

	state := tailnet.NewStateStore(&memoryStore{err: errors.New("the database is down")})
	if _, err := state.ReadState(ipn.StateKey("x")); err == nil {
		t.Error("ReadState() hid the store's error")
	}
	if err := state.WriteState(ipn.StateKey("x"), []byte("v")); err == nil {
		t.Error("WriteState() hid the store's error")
	}
}

func TestNewNodeRequiresItsOptions(t *testing.T) {
	t.Parallel()

	if _, err := tailnet.NewNode(tailnet.NodeOptions{Dir: t.TempDir()}); err == nil {
		t.Error("NewNode() accepted a node without a state store")
	}
	if _, err := tailnet.NewNode(tailnet.NodeOptions{Store: &memoryStore{}}); err == nil {
		t.Error("NewNode() accepted a node without a directory")
	}
}

func TestFakeControl(t *testing.T) {
	t.Parallel()

	ctx := context.Background()
	control := tailnet.NewFakeControl()

	key, err := control.MintKey(ctx, tailnet.KeyRequest{Hostname: "desk-1", Tags: []string{"tag:rookery"}})
	if err != nil {
		t.Fatalf("MintKey() = %v", err)
	}
	if key.Secret == "" {
		t.Fatal("MintKey() made a key without a secret")
	}
	minted := control.Minted()
	if len(minted) != 1 || minted[0].Hostname != "desk-1" {
		t.Errorf("Minted() = %+v", minted)
	}

	// A host that has not joined is not there yet.
	if _, err := control.Device(ctx, "desk-1"); !errors.Is(err, tailnet.ErrNotFound) {
		t.Errorf("Device() before joining = %v, want not found", err)
	}
	device := control.Join(tailnet.Device{
		Hostname:  "desk-1",
		Addresses: []string{"100.64.0.7"},
		Tags:      []string{"tag:rookery"},
	})
	found, err := control.Device(ctx, "desk-1")
	if err != nil {
		t.Fatalf("Device() = %v", err)
	}
	if found.ID != device.ID || found.Address() != "100.64.0.7" {
		t.Errorf("Device() = %+v", found)
	}

	devices, err := control.Devices(ctx)
	if err != nil {
		t.Fatalf("Devices() = %v", err)
	}
	if len(devices) != 1 {
		t.Errorf("Devices() = %+v, want the tagged device", devices)
	}

	if err := control.DeleteDevice(ctx, device.ID); err != nil {
		t.Fatalf("DeleteDevice() = %v", err)
	}
	if got := control.Joined(); len(got) != 0 {
		t.Errorf("Joined() = %v, want nothing", got)
	}
	if err := control.DeleteDevice(ctx, device.ID); !errors.Is(err, tailnet.ErrNotFound) {
		t.Errorf("DeleteDevice() twice = %v, want not found", err)
	}
}

func TestFakeDialer(t *testing.T) {
	t.Parallel()

	dialer := &tailnet.FakeDialer{}
	if _, err := dialer.Dial(context.Background(), "tcp", "100.64.0.7:22"); !errors.Is(err, tailnet.ErrNotFound) {
		t.Errorf("Dial() without a handler = %v, want a refusal", err)
	}

	left, right := net.Pipe()
	t.Cleanup(func() { _ = left.Close() })
	t.Cleanup(func() { _ = right.Close() })
	dialer.Handle = func(context.Context, string, string) (net.Conn, error) { return left, nil }

	conn, err := dialer.Dial(context.Background(), "tcp", "100.64.0.7:22")
	if err != nil {
		t.Fatalf("Dial() = %v", err)
	}
	if conn != left {
		t.Error("Dial() did not answer with the handler's connection")
	}
	if got := dialer.Dialed(); len(got) != 2 || got[1] != "tcp 100.64.0.7:22" {
		t.Errorf("Dialed() = %v", got)
	}
}
