package tailnet

import (
	"context"
	"errors"
	"fmt"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

// PostgresStore keeps the node's state in rookery.tailnet_state, the table the
// schema has for it. It is a Store.
type PostgresStore struct {
	pool *pgxpool.Pool
}

// NewPostgresStore keeps a tailnet node's state in the database.
func NewPostgresStore(pool *pgxpool.Pool) *PostgresStore {
	return &PostgresStore{pool: pool}
}

// Get returns the state for a key.
func (s *PostgresStore) Get(ctx context.Context, key string) ([]byte, error) {
	var value []byte
	err := s.pool.QueryRow(ctx, `SELECT value FROM rookery.tailnet_state WHERE key = $1`, key).Scan(&value)
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
		if _, err := s.pool.Exec(ctx, `DELETE FROM rookery.tailnet_state WHERE key = $1`, key); err != nil {
			return fmt.Errorf("tailnet: deleting the node's state: %w", err)
		}
		return nil
	}
	_, err := s.pool.Exec(ctx, `
		INSERT INTO rookery.tailnet_state (key, value) VALUES ($1, $2)
		ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()
	`, key, value)
	if err != nil {
		return fmt.Errorf("tailnet: writing the node's state: %w", err)
	}
	return nil
}
