// Package store owns the Postgres schema, the pool every other package uses, and
// the queries sqlc generates from those.
//
// The core schema is one file, embedded here. A database whose recorded digest is
// another one is refused rather than migrated: before the first tagged release
// there are no migrations, and Reset drops the schemas and applies the current
// file.
package store

import (
	"bytes"
	"context"
	"crypto/sha256"
	_ "embed"
	"encoding/hex"
	"errors"
	"fmt"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

// DigestKey is the rookery.meta key that holds the SHA-256 of schema.sql.
const DigestKey = "schema_digest"

// schemaLock serialises applying and dropping the schema, so that two servers
// starting at once cannot both create it.
const schemaLock int64 = 0x726f6f6b6572 // "rooker"

//go:embed schema.sql
var schemaSQL string

// ErrDigestMismatch reports a database whose schema is not the one this build
// carries.
var ErrDigestMismatch = errors.New("store: the database's schema is not this build's")

// DigestMismatchError says which digests met.
type DigestMismatchError struct {
	// Recorded is the digest the database carries, or empty when it records
	// none.
	Recorded string
	// Current is the digest of this build's schema.sql.
	Current string
}

func (e *DigestMismatchError) Error() string {
	if e.Recorded == "" {
		return fmt.Sprintf(
			"store: the database has a rookery schema but no schema digest, and this build's is %s: run `rookery server reset` to drop and recreate the schemas",
			e.Current,
		)
	}
	return fmt.Sprintf(
		"store: the database's schema digest is %s and this build's is %s: run `rookery server reset` to drop and recreate the schemas",
		e.Recorded, e.Current,
	)
}

func (e *DigestMismatchError) Unwrap() error { return ErrDigestMismatch }

// Config is what a store is opened with.
type Config struct {
	// URL is the Postgres connection string, ROOKERY_DATABASE_URL.
	URL string
	// MaxConns bounds the pool. Zero leaves pgx's default, which suits a server
	// but is too many for a test binary that opens many pools.
	MaxConns int32
}

// Store is an open pool to a Rookery database.
type Store struct {
	pool    *pgxpool.Pool
	queries *Queries
	digest  [sha256.Size]byte
}

// Open opens a pool to the database. It does not read or change the schema:
// Ensure does that.
func Open(ctx context.Context, config Config) (*Store, error) {
	settings, err := pgxpool.ParseConfig(config.URL)
	if err != nil {
		return nil, fmt.Errorf("store: parsing the database URL: %w", err)
	}
	if config.MaxConns > 0 {
		settings.MaxConns = config.MaxConns
	}

	pool, err := pgxpool.NewWithConfig(ctx, settings)
	if err != nil {
		return nil, fmt.Errorf("store: opening the pool: %w", err)
	}
	if err := pool.Ping(ctx); err != nil {
		pool.Close()
		return nil, fmt.Errorf("store: connecting to the database: %w", err)
	}

	return &Store{
		pool:    pool,
		queries: New(pool),
		digest:  sha256.Sum256([]byte(schemaSQL)),
	}, nil
}

// Pool is the connection pool, for queries that have no place in sqlc.
func (s *Store) Pool() *pgxpool.Pool { return s.pool }

// Queries are the generated queries, on the pool.
func (s *Store) Queries() *Queries { return s.queries }

// Digest is the SHA-256 of this build's schema.sql, in hex.
func (s *Store) Digest() string { return hex.EncodeToString(s.digest[:]) }

// Close closes the pool.
func (s *Store) Close() { s.pool.Close() }

// Ensure applies the schema to a database that has none, and checks the digest
// of a database that has one. A database with another digest is refused with an
// error that wraps ErrDigestMismatch.
func (s *Store) Ensure(ctx context.Context) error {
	tx, err := s.pool.Begin(ctx)
	if err != nil {
		return fmt.Errorf("store: beginning the schema transaction: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	if _, err := tx.Exec(ctx, "SELECT pg_advisory_xact_lock($1)", schemaLock); err != nil {
		return fmt.Errorf("store: taking the schema lock: %w", err)
	}

	applied, err := s.applied(ctx, tx)
	if err != nil {
		return err
	}
	if !applied {
		if _, err := tx.Exec(ctx, schemaSQL); err != nil {
			return fmt.Errorf("store: applying the schema: %w", err)
		}
		if err := New(tx).SetMeta(ctx, SetMetaParams{Key: DigestKey, Value: s.digest[:]}); err != nil {
			return fmt.Errorf("store: recording the schema digest: %w", err)
		}
		if err := tx.Commit(ctx); err != nil {
			return fmt.Errorf("store: committing the schema: %w", err)
		}
		return nil
	}

	recorded, err := New(tx).GetMeta(ctx, DigestKey)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return &DigestMismatchError{Current: s.Digest()}
	case err != nil:
		return fmt.Errorf("store: reading the schema digest: %w", err)
	case !bytes.Equal(recorded, s.digest[:]):
		return &DigestMismatchError{Recorded: hex.EncodeToString(recorded), Current: s.Digest()}
	}

	if err := tx.Commit(ctx); err != nil {
		return fmt.Errorf("store: committing the schema check: %w", err)
	}
	return nil
}

// applied reports whether the database has the core schema, creating nothing. A
// rookery schema without a meta table is not applied: it is a schema something
// else made, and Reset is the way out.
func (s *Store) applied(ctx context.Context, tx pgx.Tx) (bool, error) {
	var hasTable, hasSchema bool
	err := tx.QueryRow(ctx, `
		SELECT to_regclass('rookery.meta') IS NOT NULL,
		       EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = 'rookery')
	`).Scan(&hasTable, &hasSchema)
	if err != nil {
		return false, fmt.Errorf("store: checking for the schema: %w", err)
	}
	if hasTable {
		return true, nil
	}
	if hasSchema {
		return false, &DigestMismatchError{Current: s.Digest()}
	}
	return false, nil
}

// Reset drops the core schema and applies this build's. It is what
// `rookery server reset` runs after asking.
//
// Plugin schemas, and the roles that own them, are dropped here too once the
// plugin system exists.
func (s *Store) Reset(ctx context.Context) error {
	tx, err := s.pool.Begin(ctx)
	if err != nil {
		return fmt.Errorf("store: beginning the reset transaction: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	if _, err := tx.Exec(ctx, "SELECT pg_advisory_xact_lock($1)", schemaLock); err != nil {
		return fmt.Errorf("store: taking the schema lock: %w", err)
	}
	if _, err := tx.Exec(ctx, `DROP SCHEMA IF EXISTS rookery CASCADE`); err != nil {
		return fmt.Errorf("store: dropping the schema: %w", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return fmt.Errorf("store: committing the reset: %w", err)
	}

	return s.Ensure(ctx)
}
