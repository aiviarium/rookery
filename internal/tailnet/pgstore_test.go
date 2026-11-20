package tailnet_test

import (
	"context"
	"errors"
	"testing"

	"github.com/aiviarium/rookery/internal/store/storetest"
	"github.com/aiviarium/rookery/internal/tailnet"
)

func TestPostgresStore(t *testing.T) {
	t.Parallel()

	store := storetest.New(t)
	state := tailnet.NewPostgresStore(store.Pool())
	ctx := context.Background()

	// A node that has never run has no state, which is what makes tsnet join
	// with the auth key the server gives it.
	if _, err := state.Get(ctx, "_machinekey"); !errors.Is(err, tailnet.ErrNoState) {
		t.Fatalf("Get() = %v, want no state", err)
	}

	if err := state.Set(ctx, "_machinekey", []byte("mkey:abc")); err != nil {
		t.Fatalf("Set() = %v", err)
	}
	value, err := state.Get(ctx, "_machinekey")
	if err != nil {
		t.Fatalf("Get() = %v", err)
	}
	if string(value) != "mkey:abc" {
		t.Errorf("Get() = %q", value)
	}

	// Writing again replaces, rather than adding a second row.
	if err := state.Set(ctx, "_machinekey", []byte("mkey:def")); err != nil {
		t.Fatalf("Set() = %v", err)
	}
	value, err = state.Get(ctx, "_machinekey")
	if err != nil {
		t.Fatalf("Get() = %v", err)
	}
	if string(value) != "mkey:def" {
		t.Errorf("Get() = %q, want the new value", value)
	}

	if err := state.Set(ctx, "_machinekey", nil); err != nil {
		t.Fatalf("Set(nil) = %v", err)
	}
	if _, err := state.Get(ctx, "_machinekey"); !errors.Is(err, tailnet.ErrNoState) {
		t.Errorf("Get() after deleting = %v, want no state", err)
	}
}

// TestPostgresStoreHoldsTheNodeState is the adapter and the table together: what
// tsnet writes is what a later server reads.
func TestPostgresStoreHoldsTheNodeState(t *testing.T) {
	t.Parallel()

	store := storetest.New(t)
	state := tailnet.NewStateStore(tailnet.NewPostgresStore(store.Pool()))

	if err := state.WriteState("_machinekey", []byte("mkey:abc")); err != nil {
		t.Fatalf("WriteState() = %v", err)
	}
	// A second adapter, as a standby has after it takes the lead.
	other := tailnet.NewStateStore(tailnet.NewPostgresStore(store.Pool()))
	value, err := other.ReadState("_machinekey")
	if err != nil {
		t.Fatalf("ReadState() = %v", err)
	}
	if string(value) != "mkey:abc" {
		t.Errorf("ReadState() = %q", value)
	}
}
