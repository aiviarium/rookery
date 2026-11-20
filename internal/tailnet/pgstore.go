package tailnet

import (
	"context"
	"errors"
	"fmt"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"

	"github.com/aiviarium/rookery/internal/store"
)

// PostgresStore keeps the node's state in rookery.tailnet_state, the table the
// schema has for it. It is a Store.
type PostgresStore struct {
	queries *store.Queries
}

// NewPostgresStore keeps a tailnet node's state in the database.
func NewPostgresStore(pool *pgxpool.Pool) *PostgresStore {
	return &PostgresStore{queries: store.New(pool)}
}

// Get returns the state for a key.
func (s *PostgresStore) Get(ctx context.Context, key string) ([]byte, error) {
	value, err := s.queries.GetTailnetState(ctx, key)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return nil, fmt.Errorf("%w: %q", ErrNoState, key)
	case err != nil:
		return nil, fmt.Errorf("tailnet: reading the node's state: %w", err)
	}
	return value, nil
}

// Set writes the state for a key, and deletes it when value is nil.
func (s *PostgresStore) Set(ctx context.Context, key string, value []byte) error {
	if value == nil {
		if err := s.queries.DeleteTailnetState(ctx, key); err != nil {
			return fmt.Errorf("tailnet: deleting the node's state: %w", err)
		}
		return nil
	}
	err := s.queries.PutTailnetState(ctx, store.PutTailnetStateParams{Key: key, Value: value})
	if err != nil {
		return fmt.Errorf("tailnet: writing the node's state: %w", err)
	}
	return nil
}
