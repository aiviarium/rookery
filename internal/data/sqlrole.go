package data

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"errors"
	"fmt"
	"net/url"
	"strings"

	"github.com/jackc/pgx/v5"
)

// Sealer seals a secret under the deployment's key, as internal/seal's Ring
// does: the plugin's database password is kept sealed in rookery.sql_roles like
// every other secret.
type Sealer interface {
	// Seal encrypts plain, bound to associated, and names the key that sealed
	// it.
	Seal(plain, associated []byte) (sealed []byte, keyID string, err error)

	// Open decrypts a value sealed under the named key.
	Open(sealed []byte, keyID string, associated []byte) ([]byte, error)
}

// EnsureSQLRole gives a plugin that declares `database: sql` the role and schema
// its raw SQL runs in, and returns the connection string for it.
//
// The role is p_<plugin>_sql and owns the schema of the same name. It may read
// the plugin's own collections and nothing else: no core table, and no other
// plugin's. Its password is made once, sealed under the deployment's key, and
// kept, so that a plugin's saved connection string keeps working. Calling this
// again restores what a reset dropped.
//
// The server's database user needs CREATEROLE.
func (d *Data) EnsureSQLRole(ctx context.Context, plugin string) (string, error) {
	return d.ensureSQLRole(ctx, plugin, d.transaction)
}

// EnsureSQLRoleIn creates the raw SQL role in the install transaction.
func (d *Data) EnsureSQLRoleIn(ctx context.Context, tx pgx.Tx, plugin string) error {
	_, err := d.ensureSQLRole(ctx, plugin, func(_ context.Context, fn func(pgx.Tx) error) error {
		return fn(tx) //nolint:contextcheck // the callback encloses the caller's context
	})
	return err
}

func (d *Data) ensureSQLRole(ctx context.Context, plugin string, transaction func(context.Context, func(pgx.Tx) error) error) (string, error) {
	if !namePattern.MatchString(plugin) {
		return "", fmt.Errorf("%w: %q is not a plugin id", ErrInvalid, plugin)
	}
	if d.databaseURL == "" {
		return "", fmt.Errorf("%w: the datastore has no database URL for a plugin's connection", ErrInvalid)
	}
	if d.sealer == nil {
		return "", fmt.Errorf("%w: the datastore has no sealer for a plugin's password", ErrInvalid)
	}

	role := "p_" + plugin + "_sql"
	schema := role

	var password string
	err := transaction(ctx, func(tx pgx.Tx) error {
		// One call at a time makes a role, so that two installs of one plugin
		// do not make two passwords.
		if _, err := tx.Exec(ctx, "SELECT pg_advisory_xact_lock(hashtext($1))", "rookery.sqlrole:"+plugin); err != nil {
			return fmt.Errorf("data: locking the sql role of %s: %w", plugin, err)
		}

		var sealed []byte
		var keyID string
		err := tx.QueryRow(ctx, "SELECT password, key_id FROM rookery.sql_roles WHERE plugin = $1", plugin).
			Scan(&sealed, &keyID)
		switch {
		case errors.Is(err, pgx.ErrNoRows):
			plain, err := newPassword()
			if err != nil {
				return err
			}
			if err := createRole(ctx, tx, role, plain, false); err != nil {
				return err
			}
			sealed, keyID, err = d.sealer.Seal([]byte(plain), []byte(plugin))
			if err != nil {
				return fmt.Errorf("data: sealing the password of %s: %w", plugin, err)
			}
			_, err = tx.Exec(ctx, `INSERT INTO rookery.sql_roles (plugin, role, password, key_id)
				VALUES ($1, $2, $3, $4)`, plugin, role, sealed, keyID)
			if err != nil {
				return fmt.Errorf("data: keeping the sql role of %s: %w", plugin, err)
			}
			password = plain
		case err != nil:
			return fmt.Errorf("data: reading the sql role of %s: %w", plugin, err)
		default:
			plain, err := d.sealer.Open(sealed, keyID, []byte(plugin))
			if err != nil {
				return fmt.Errorf("data: opening the password of %s: %w", plugin, err)
			}
			password = string(plain)
			// A reset may have dropped the role; restore it under the password
			// the plugin already has.
			if err := createRole(ctx, tx, role, password, true); err != nil {
				return err
			}
		}

		return grantPluginSQL(ctx, tx, plugin, role, schema)
	})
	if err != nil {
		return "", err
	}
	return d.pluginDSN(role, password)
}

// SQLCredentials is the connection string of a plugin's raw SQL role, as its
// service gets it.
func (d *Data) SQLCredentials(ctx context.Context, plugin string) (string, error) {
	if d.sealer == nil {
		return "", fmt.Errorf("%w: the datastore has no sealer for a plugin's password", ErrInvalid)
	}
	var sealed []byte
	var keyID, role string
	err := d.pool().QueryRow(ctx, "SELECT role, password, key_id FROM rookery.sql_roles WHERE plugin = $1", plugin).
		Scan(&role, &sealed, &keyID)
	if err != nil {
		return "", fmt.Errorf("%w: the plugin %s has no sql role", ErrNotFound, plugin)
	}
	plain, err := d.sealer.Open(sealed, keyID, []byte(plugin))
	if err != nil {
		return "", fmt.Errorf("data: opening the password of %s: %w", plugin, err)
	}
	return d.pluginDSN(role, string(plain))
}

// DropSQLRole removes a plugin's raw SQL role, its schema and its kept password.
// The plugin system runs it when a plugin is removed with its data.
func (d *Data) DropSQLRole(ctx context.Context, plugin string) error {
	if !namePattern.MatchString(plugin) {
		return fmt.Errorf("%w: %q is not a plugin id", ErrInvalid, plugin)
	}
	role := "p_" + plugin + "_sql"

	return d.transaction(ctx, func(tx pgx.Tx) error {
		quoted := pgIdentifier(role)
		if _, err := tx.Exec(ctx, "DROP SCHEMA IF EXISTS "+quoted+" CASCADE"); err != nil {
			return fmt.Errorf("data: dropping the sql schema of %s: %w", plugin, err)
		}

		var present bool
		err := tx.QueryRow(ctx, "SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)", role).Scan(&present)
		if err != nil {
			return fmt.Errorf("data: looking for the role %s: %w", role, err)
		}
		if present {
			for _, statement := range []string{"DROP OWNED BY " + quoted, "DROP ROLE " + quoted} {
				if _, err := tx.Exec(ctx, statement); err != nil {
					return fmt.Errorf("data: dropping the sql role of %s: %w", plugin, err)
				}
			}
		}
		if _, err := tx.Exec(ctx, "DELETE FROM rookery.sql_roles WHERE plugin = $1", plugin); err != nil {
			return fmt.Errorf("data: forgetting the sql role of %s: %w", plugin, err)
		}
		return nil
	})
}

// createRole makes the role, or gives an existing one the password it already
// had. A role's password cannot be a parameter, so it goes in as a quoted
// literal; it is base64url, and quoted regardless.
func createRole(ctx context.Context, tx pgx.Tx, role, password string, existing bool) error {
	quoted := pgIdentifier(role)
	if !existing {
		var present bool
		err := tx.QueryRow(ctx, "SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)", role).Scan(&present)
		if err != nil {
			return fmt.Errorf("data: looking for the role %s: %w", role, err)
		}
		if !present {
			statement := "CREATE ROLE " + quoted + " LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD " + quoteLiteral(password)
			if _, err := tx.Exec(ctx, statement); err != nil {
				return fmt.Errorf("data: creating the role %s (the server's database user needs CREATEROLE): %w", role, err)
			}
			return nil
		}
	}
	statement := "ALTER ROLE " + quoted + " WITH LOGIN PASSWORD " + quoteLiteral(password)
	if _, err := tx.Exec(ctx, statement); err != nil {
		return fmt.Errorf("data: setting the password of the role %s: %w", role, err)
	}
	return nil
}

// grantPluginSQL gives the role its schema and reading rights on the plugin's
// own collections.
func grantPluginSQL(ctx context.Context, tx pgx.Tx, plugin, role, schema string) error {
	quotedRole := pgIdentifier(role)
	collections := pgIdentifier("p_" + plugin)

	statements := []string{
		"CREATE SCHEMA IF NOT EXISTS " + collections,
		"CREATE SCHEMA IF NOT EXISTS " + pgIdentifier(schema) + " AUTHORIZATION " + quotedRole,
		"ALTER SCHEMA " + pgIdentifier(schema) + " OWNER TO " + quotedRole,
		"GRANT USAGE ON SCHEMA " + collections + " TO " + quotedRole,
		"GRANT SELECT ON ALL TABLES IN SCHEMA " + collections + " TO " + quotedRole,
		"ALTER DEFAULT PRIVILEGES IN SCHEMA " + collections + " GRANT SELECT ON TABLES TO " + quotedRole,
	}
	for _, statement := range statements {
		if _, err := tx.Exec(ctx, statement); err != nil {
			return fmt.Errorf("data: granting the sql role of %s its schema: %w", plugin, err)
		}
	}
	return nil
}

// pluginDSN is the connection string a plugin's role uses: the server's own with
// its credentials replaced.
func (d *Data) pluginDSN(role, password string) (string, error) {
	parsed, err := url.Parse(d.databaseURL)
	if err != nil {
		return "", fmt.Errorf("data: reading the database URL: %w", err)
	}
	parsed.User = url.UserPassword(role, password)
	return parsed.String(), nil
}

// newPassword makes a role's password: 32 random bytes in base64url, which needs
// no escape in a statement or a URL.
func newPassword() (string, error) {
	raw := make([]byte, 32)
	if _, err := rand.Read(raw); err != nil {
		return "", fmt.Errorf("data: reading a password: %w", err)
	}
	return base64.RawURLEncoding.EncodeToString(raw), nil
}

// quoteLiteral quotes a string for a statement that cannot take a parameter.
func quoteLiteral(text string) string {
	return "'" + strings.ReplaceAll(text, "'", "''") + "'"
}
