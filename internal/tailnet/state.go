package tailnet

import (
	"context"
	"errors"
	"fmt"
	"time"

	"tailscale.com/ipn"
)

// Store keeps a tailnet node's state: its keys, its preferences and what the
// control server told it. What it holds is what makes the node the same node
// after a restart, so a standby that takes the lead needs none of the previous
// server's memory, only this.
type Store interface {
	// Get returns the state for key, or ErrNoState when there is none.
	Get(ctx context.Context, key string) ([]byte, error)
	// Set writes the state for key, and deletes it when value is nil.
	Set(ctx context.Context, key string, value []byte) error
}

// stateTimeout bounds one read or write of the state: tsnet asks for it while it
// starts and while it saves preferences, without a context of its own.
const stateTimeout = 30 * time.Second

// NewStateStore is a Store as tsnet asks for it, which is what the server's node
// keeps its state in.
func NewStateStore(store Store) ipn.StateStore {
	return &nodeState{store: store}
}

// nodeState is a Store as tsnet asks for it, which is without a context.
type nodeState struct {
	store Store
}

// ReadState reads the state for a key.
func (s *nodeState) ReadState(key ipn.StateKey) ([]byte, error) {
	ctx, cancel := context.WithTimeout(context.Background(), stateTimeout)
	defer cancel()

	value, err := s.store.Get(ctx, string(key))
	switch {
	case errors.Is(err, ErrNoState):
		return nil, ipn.ErrStateNotExist
	case err != nil:
		return nil, fmt.Errorf("tailnet: reading the node's state: %w", err)
	}
	return value, nil
}

// WriteState writes the state for a key, and deletes it when value is nil.
func (s *nodeState) WriteState(key ipn.StateKey, value []byte) error {
	ctx, cancel := context.WithTimeout(context.Background(), stateTimeout)
	defer cancel()

	if err := s.store.Set(ctx, string(key), value); err != nil {
		return fmt.Errorf("tailnet: writing the node's state: %w", err)
	}
	return nil
}
