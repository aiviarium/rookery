// Package storetest gives a test its own Postgres database.
//
// The first call in a test binary applies schema.sql to a template database
// named after the binary, and every test then gets a clone of it, dropped when
// the test ends. Cloning is why the template exists: creating a database from a
// template is far cheaper than applying the schema.
//
// Everything reads ROOKERY_TEST_POSTGRES, the connection string of a Postgres
// with pgvector whose user may create databases. Unset, New skips the test.
// Unset in CI, and the test fails instead: a skipped store test in CI is a green
// run that tested nothing.
package storetest

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"net/url"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgxpool"

	"github.com/aiviarium/rookery/internal/store"
)

// URLEnv is the variable that holds the connection string of the Postgres the
// tests clone databases on.
const URLEnv = "ROOKERY_TEST_POSTGRES"

const (
	templatePrefix = "rookery_tpl_"
	databasePrefix = "rookery_test_"

	// maxConns bounds each test's pool. Tests run in parallel and each holds a
	// pool; a server-sized pool each would exhaust the database's connections
	// long before the tests end.
	maxConns = 4
)

// New returns a store on a database of the test's own: a clone of the test
// binary's template, dropped when the test ends.
func New(t *testing.T) *store.Store {
	t.Helper()

	raw := os.Getenv(URLEnv)
	if raw == "" {
		if os.Getenv("CI") != "" {
			t.Fatalf("storetest: %s is not set and CI is set", URLEnv)
		}
		t.Skipf("storetest: %s is not set", URLEnv)
	}

	ctx := t.Context()
	admin, err := adminPool()
	if err != nil {
		t.Fatalf("storetest: %v", err)
	}

	template := templateName(raw)
	if err := ensureTemplate(ctx, admin, raw, template); err != nil {
		t.Fatalf("storetest: %v", err)
	}

	cloneName := databasePrefix + randomHex(6)
	cloneURL, err := withDatabase(raw, cloneName)
	if err != nil {
		t.Fatalf("storetest: %v", err)
	}

	conn, err := admin.Acquire(ctx)
	if err != nil {
		t.Fatalf("storetest: acquiring an admin connection: %v", err)
	}
	// Shared with the other tests of this binary: clones run together, but no
	// clone starts while the template is being made.
	if _, err := conn.Exec(ctx, "SELECT pg_advisory_lock_shared(hashtext($1))", lockKey(template)); err != nil {
		conn.Release()
		t.Fatalf("storetest: taking the template lock: %v", err)
	}
	err = createDatabase(ctx, conn, cloneName, template)
	_, _ = conn.Exec(context.WithoutCancel(ctx), "SELECT pg_advisory_unlock_shared(hashtext($1))", lockKey(template))
	conn.Release()
	if err != nil {
		t.Fatalf("storetest: %v", err)
	}
	t.Cleanup(func() { dropTestDatabase(t, cloneName) })

	clone, err := store.Open(ctx, store.Config{URL: cloneURL, MaxConns: maxConns})
	if err != nil {
		t.Fatalf("storetest: %v", err)
	}
	t.Cleanup(clone.Close)

	return clone
}

// adminPool is one pool for creating and dropping databases, shared by every
// test in the binary: a pool and a connection per test would exhaust the
// server's connections. It is the only thing here that outlives a test.
var adminPool = sync.OnceValues(func() (*pgxpool.Pool, error) {
	setup, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	pool, err := pgxpool.New(setup, os.Getenv(URLEnv))
	if err != nil {
		return nil, fmt.Errorf("opening the admin pool: %w", err)
	}
	if err := pool.Ping(setup); err != nil {
		pool.Close()
		return nil, fmt.Errorf("connecting the admin pool: %w", err)
	}
	return pool, nil
})

// ensureTemplate applies schema.sql to the template database, or recreates a
// template whose schema is not this build's.
func ensureTemplate(ctx context.Context, admin *pgxpool.Pool, raw, template string) error {
	conn, err := admin.Acquire(ctx)
	if err != nil {
		return fmt.Errorf("acquiring an admin connection: %w", err)
	}
	defer conn.Release()

	// One test binary at a time creates its template. Clones take the same lock
	// shared, so they run together, but never while the template is written.
	key := lockKey(template)
	if _, err := conn.Exec(ctx, "SELECT pg_advisory_lock(hashtext($1))", key); err != nil {
		return fmt.Errorf("taking the template lock: %w", err)
	}
	defer func() {
		_, _ = conn.Exec(context.WithoutCancel(ctx), "SELECT pg_advisory_unlock(hashtext($1))", key)
	}()

	exists, err := databaseExists(ctx, conn, template)
	if err != nil {
		return err
	}
	if exists {
		stale, err := templateIsStale(ctx, raw, template)
		if err != nil {
			return err
		}
		if !stale {
			return nil
		}
		if err := dropDatabase(ctx, conn, template); err != nil {
			return err
		}
	}

	if err := createDatabase(ctx, conn, template, ""); err != nil {
		return err
	}
	return applySchema(ctx, raw, template)
}

// applySchema opens the template and applies schema.sql to it, closing the pool
// before anyone clones the template.
func applySchema(ctx context.Context, raw, template string) error {
	templateURL, err := withDatabase(raw, template)
	if err != nil {
		return err
	}
	created, err := store.Open(ctx, store.Config{URL: templateURL, MaxConns: 1})
	if err != nil {
		return fmt.Errorf("opening the template: %w", err)
	}
	defer created.Close()

	if err := created.Ensure(ctx); err != nil {
		return fmt.Errorf("applying the schema to the template: %w", err)
	}
	return nil
}

// templateIsStale reports whether the template carries another schema than this
// build's.
func templateIsStale(ctx context.Context, raw, template string) (bool, error) {
	templateURL, err := withDatabase(raw, template)
	if err != nil {
		return false, err
	}
	opened, err := store.Open(ctx, store.Config{URL: templateURL, MaxConns: 1})
	if err != nil {
		return false, fmt.Errorf("opening the template: %w", err)
	}
	defer opened.Close()

	switch err := opened.Ensure(ctx); {
	case err == nil:
		return false, nil
	case errors.Is(err, store.ErrDigestMismatch):
		return true, nil
	default:
		return false, fmt.Errorf("checking the template: %w", err)
	}
}

// databaseExists reports whether a database of that name is there.
func databaseExists(ctx context.Context, conn *pgxpool.Conn, name string) (bool, error) {
	var exists bool
	err := conn.QueryRow(ctx, `SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)`, name).Scan(&exists)
	if err != nil {
		return false, fmt.Errorf("looking for the database %s: %w", name, err)
	}
	return exists, nil
}

// createDatabase creates a database, from a template when one is named. A
// template is cloned only while no session is connected to it; a connection this
// binary has just closed can take a moment to go, so a refusal is retried once
// after clearing the template's sessions.
func createDatabase(ctx context.Context, conn *pgxpool.Conn, name, template string) error {
	query := "CREATE DATABASE " + quoteIdentifier(name)
	if template != "" {
		query += " TEMPLATE " + quoteIdentifier(template)
	}

	_, err := conn.Exec(ctx, query)
	if err == nil {
		return nil
	}
	if template == "" || !strings.Contains(err.Error(), "is being accessed") {
		return fmt.Errorf("creating the database %s: %w", name, err)
	}

	_, _ = conn.Exec(ctx, `
		SELECT pg_terminate_backend(pid) FROM pg_stat_activity
		WHERE datname = $1 AND pid <> pg_backend_pid()`, template)
	if _, retried := conn.Exec(ctx, query); retried != nil {
		return fmt.Errorf("creating the database %s from %s: %w", name, template, retried)
	}
	return nil
}

// execer runs a statement on one connection. A pooled connection and a plain
// one both are one.
type execer interface {
	Exec(ctx context.Context, sql string, arguments ...any) (pgconn.CommandTag, error)
}

// dropDatabase drops a database and every session still on it.
func dropDatabase(ctx context.Context, conn execer, name string) error {
	_, err := conn.Exec(ctx, "DROP DATABASE IF EXISTS "+quoteIdentifier(name)+" WITH (FORCE)")
	if err != nil {
		return fmt.Errorf("dropping the database %s: %w", name, err)
	}
	return nil
}

// dropTestDatabase drops a test's database. It runs after the test's context is
// canceled, so it has a context of its own.
func dropTestDatabase(t *testing.T, name string) {
	t.Helper()

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	admin, err := adminPool()
	if err != nil {
		t.Logf("storetest: dropping %s: %v", name, err)
		return
	}
	conn, err := admin.Acquire(ctx)
	if err != nil {
		t.Logf("storetest: dropping %s: %v", name, err)
		return
	}
	defer conn.Release()

	if err := dropDatabase(ctx, conn, name); err != nil {
		t.Logf("storetest: %v", err)
	}
}

// templateName is the template's name: the URL and the test binary's path, so
// that parallel packages and parallel databases never share a template.
func templateName(raw string) string {
	sum := sha256.Sum256([]byte(raw + "\x00" + os.Args[0]))
	return templatePrefix + hex.EncodeToString(sum[:6])
}

// lockKey is the advisory lock's key for a template, from its name.
func lockKey(template string) string { return "rookery.storetest:" + template }

// randomHex returns n random bytes in hex.
func randomHex(n int) string {
	buf := make([]byte, n)
	if _, err := rand.Read(buf); err != nil {
		panic(fmt.Sprintf("storetest: reading random bytes: %v", err))
	}
	return hex.EncodeToString(buf)
}

// withDatabase is a connection string for another database on the same server.
func withDatabase(raw, name string) (string, error) {
	parsed, err := url.Parse(raw)
	if err != nil {
		return "", fmt.Errorf("parsing %s: %w", URLEnv, err)
	}
	parsed.Path = "/" + name
	return parsed.String(), nil
}

// quoteIdentifier quotes a database name for a statement that cannot take a
// parameter.
func quoteIdentifier(name string) string {
	return `"` + strings.ReplaceAll(name, `"`, `""`) + `"`
}
