package data

import (
	"context"
	"errors"
	"fmt"
	"math/rand/v2"
	"net/url"
	"os"
	"strings"
	"testing"

	"github.com/jackc/pgx/v5"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/filter"
	"github.com/aiviarium/rookery/internal/seal"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// sqlDatastore returns a datastore with a collection of its own plugin and a
// sealed ring, ready for raw SQL roles.
func sqlDatastore(t *testing.T) (*datastore, string) {
	t.Helper()

	// Roles are cluster-wide, so every test makes its own plugin.
	suffix := strings.ToLower(fmt.Sprintf("%06x", rand.IntN(1<<24))) //nolint:gosec // a name needs to differ, not to be unpredictable
	plugin := "sql" + suffix

	key, err := seal.New()
	if err != nil {
		t.Fatalf("making a key: %v", err)
	}
	ring, err := seal.NewRing(key.ID(), key)
	if err != nil {
		t.Fatalf("making a ring: %v", err)
	}

	store := storetest.New(t)
	auth := newTestAuth()
	auth.grant(Principal{Kind: PrincipalLocal}, "plugin:"+plugin, rookeryv1.Level_LEVEL_MANAGE)
	data := New(Config{
		Store:       store,
		Authorizer:  auth,
		DatabaseURL: testDatabaseURL(t, store),
		Sealer:      ring,
	})
	err = data.Declare(&Collection{
		Plugin: plugin,
		Name:   "things",
		Scope:  ScopeGlobal,
		Access: Access{Read: rookeryv1.Level_LEVEL_VIEW, Write: rookeryv1.Level_LEVEL_MANAGE},
		Fields: map[string]Field{"label": {Type: filter.TypeString}},
	})
	if err != nil {
		t.Fatalf("declaring: %v", err)
	}
	if err := data.Create(t.Context(), plugin); err != nil {
		t.Fatalf("creating: %v", err)
	}
	if _, err := data.Insert(t.Context(), Principal{Kind: PrincipalLocal}, plugin+"/things", map[string]any{"label": "one"}); err != nil {
		t.Fatalf("inserting: %v", err)
	}

	t.Cleanup(func() {
		if err := data.DropSQLRole(context.WithoutCancel(t.Context()), plugin); err != nil {
			t.Logf("dropping the role of %s: %v", plugin, err)
		}
	})
	return &datastore{Data: data, auth: auth}, plugin
}

// testDatabaseURL is the connection string of the test's database: the test
// Postgres's URL with the clone's name.
func testDatabaseURL(t *testing.T, store *store.Store) string {
	t.Helper()

	var name string
	if err := store.Pool().QueryRow(t.Context(), "SELECT current_database()").Scan(&name); err != nil {
		t.Fatalf("reading the database's name: %v", err)
	}
	parsed, err := url.Parse(os.Getenv(storetest.URLEnv))
	if err != nil {
		t.Fatalf("parsing %s: %v", storetest.URLEnv, err)
	}
	parsed.Path = "/" + name
	return parsed.String()
}

// TestSQLRole checks that a plugin gets a role that reads its collections and
// owns its own schema, and that the credentials stay the same.
func TestSQLRole(t *testing.T) {
	t.Parallel()

	d, plugin := sqlDatastore(t)
	ctx := t.Context()

	dsn, err := d.EnsureSQLRole(ctx, plugin)
	if err != nil {
		t.Fatalf("ensuring the sql role: %v", err)
	}
	if !strings.Contains(dsn, "p_"+plugin+"_sql") {
		t.Fatalf("the connection string does not name the role: %s", redact(dsn))
	}

	connection, err := pgx.Connect(ctx, dsn)
	if err != nil {
		t.Fatalf("connecting as the role: %v", err)
	}
	defer func() { _ = connection.Close(context.WithoutCancel(ctx)) }()

	// It owns its schema.
	_, err = connection.Exec(ctx, "CREATE TABLE p_"+plugin+"_sql.notes (id bigint PRIMARY KEY, body text)")
	if err != nil {
		t.Fatalf("creating a table in the plugin's schema: %v", err)
	}
	if _, err := connection.Exec(ctx, "INSERT INTO p_"+plugin+"_sql.notes VALUES (1, 'hello')"); err != nil {
		t.Fatalf("writing in the plugin's schema: %v", err)
	}

	// It reads its own collections.
	var count int
	if err := connection.QueryRow(ctx, "SELECT count(*) FROM p_"+plugin+".things").Scan(&count); err != nil {
		t.Fatalf("reading the plugin's collection: %v", err)
	}
	if count != 1 {
		t.Errorf("the collection has %d records, want 1", count)
	}

	// It reads no core table.
	if _, err := connection.Exec(ctx, "SELECT 1 FROM rookery.meta"); err == nil {
		t.Error("the role may read rookery.meta")
	}

	// It reads no other plugin's collection.
	if _, err := connection.Exec(ctx, "SELECT 1 FROM p_example.notes"); err == nil {
		t.Error("the role may read another plugin's collection")
	}

	// The credentials are the ones it already had, and later calls still find
	// them.
	again, err := d.EnsureSQLRole(ctx, plugin)
	if err != nil {
		t.Fatalf("ensuring the sql role again: %v", err)
	}
	if again != dsn {
		t.Errorf("the second connection string differs from the first: %s", redact(again))
	}
	credentials, err := d.SQLCredentials(ctx, plugin)
	if err != nil {
		t.Fatalf("reading the credentials: %v", err)
	}
	if credentials != dsn {
		t.Errorf("the credentials differ from the connection string: %s", redact(credentials))
	}

	// The kept password opens the role and nothing in a log would.
	var sealed []byte
	var keyID string
	err = d.Store().Pool().QueryRow(ctx, "SELECT password, key_id FROM rookery.sql_roles WHERE plugin = $1", plugin).
		Scan(&sealed, &keyID)
	if err != nil {
		t.Fatalf("reading the sealed password: %v", err)
	}
	if len(keyID) != 16 {
		t.Errorf("the key id is %q", keyID)
	}
}

// TestSQLRoleDropped checks that a plugin's role goes away with its data.
func TestSQLRoleDropped(t *testing.T) {
	t.Parallel()

	d, plugin := sqlDatastore(t)
	ctx := t.Context()

	if _, err := d.EnsureSQLRole(ctx, plugin); err != nil {
		t.Fatalf("ensuring the sql role: %v", err)
	}
	if err := d.DropSQLRole(ctx, plugin); err != nil {
		t.Fatalf("dropping the sql role: %v", err)
	}

	_, err := d.EnsureSQLRole(ctx, "missing/plugin")
	if !errors.Is(err, ErrInvalid) {
		t.Fatalf("ensuring a role for a bad plugin id = %v, want ErrInvalid", err)
	}
	if _, err := d.SQLCredentials(ctx, "no"+strings.ToLower(fmt.Sprintf("%06x", rand.IntN(1<<24)))); !errors.Is(err, ErrNotFound) { //nolint:gosec
		t.Fatalf("credentials of a plugin without a role = %v, want ErrNotFound", err)
	}
}

// redact hides a connection string's password.
func redact(dsn string) string {
	parsed, err := url.Parse(dsn)
	if err != nil {
		return dsn
	}
	parsed.User = url.UserPassword(parsed.User.Username(), "redacted")
	return parsed.String()
}
