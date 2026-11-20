package data

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"strings"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/filter"
)

// Create makes a plugin's schema and the tables its collections need. The
// plugin system calls it at install. The declaration and its table are
// committed together, and creating the same collection again is safe.
func (d *Data) Create(ctx context.Context, plugin string) error {
	return d.writeTransaction(ctx, func(tx pgx.Tx) error {
		return CreateIn(ctx, tx, plugin, d.Collections(plugin))
	})
}

// CreateIn creates and records collections in the install transaction.
func CreateIn(ctx context.Context, tx pgx.Tx, plugin string, collections []*Collection) error {
	if len(collections) == 0 {
		return nil
	}
	schema := pgIdentifier("p_" + plugin)
	if _, err := tx.Exec(ctx, "CREATE SCHEMA IF NOT EXISTS "+schema); err != nil {
		return fmt.Errorf("data: creating the schema of %s: %w", plugin, err)
	}
	for _, collection := range collections {
		if err := storeCollection(ctx, tx, collection); err != nil {
			return err
		}
	}
	return nil
}

func storeCollection(ctx context.Context, tx pgx.Tx, collection *Collection) error {
	declaration, err := cborx.Marshal(collection)
	if err != nil {
		return fmt.Errorf("data: encoding %s: %w", collection.Key(), err)
	}
	var current []byte
	err = tx.QueryRow(ctx, "SELECT declaration FROM rookery.collections WHERE plugin = $1 AND name = $2",
		collection.Plugin, collection.Name).Scan(&current)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		if _, err := tx.Exec(ctx, "INSERT INTO rookery.collections (plugin, name, declaration) VALUES ($1, $2, $3)",
			collection.Plugin, collection.Name, declaration); err != nil {
			return fmt.Errorf("data: recording %s: %w", collection.Key(), err)
		}
		return createCollection(ctx, tx, collection)
	case err != nil:
		return fmt.Errorf("data: reading the declaration of %s: %w", collection.Key(), err)
	case !bytes.Equal(current, declaration):
		return fmt.Errorf("%w: %s changed its stored declaration and needs a data reset", ErrRevision, collection.Key())
	default:
		return nil
	}
}

// Load repopulates the in-memory registry from the persisted schema catalog.
func (d *Data) Load(ctx context.Context) error {
	rows, err := d.pool().Query(ctx, "SELECT declaration FROM rookery.collections")
	if err != nil {
		return fmt.Errorf("data: loading collection declarations: %w", err)
	}
	defer rows.Close()
	var collections []*Collection
	for rows.Next() {
		var encoded []byte
		if err := rows.Scan(&encoded); err != nil {
			return fmt.Errorf("data: reading a collection declaration: %w", err)
		}
		var collection Collection
		if err := cborx.Unmarshal(encoded, &collection); err != nil {
			return fmt.Errorf("data: decoding a collection declaration: %w", err)
		}
		collections = append(collections, &collection)
	}
	if err := rows.Err(); err != nil {
		return fmt.Errorf("data: loading collection declarations: %w", err)
	}
	return d.Declare(collections...)
}

// Drop removes a plugin's schema, catalog entries and derived data.
func (d *Data) Drop(ctx context.Context, plugin string) error {
	if err := d.writeTransaction(ctx, func(tx pgx.Tx) error { return DropIn(ctx, tx, plugin) }); err != nil {
		return err
	}
	d.Forget(plugin)
	return nil
}

// DropIn removes collection tables and derived records in the install transaction.
func DropIn(ctx context.Context, tx pgx.Tx, plugin string) error {
	if !namePattern.MatchString(plugin) {
		return fmt.Errorf("%w: invalid plugin %q", ErrInvalid, plugin)
	}
	if _, err := tx.Exec(ctx, "DROP SCHEMA IF EXISTS "+pgIdentifier("p_"+plugin)+" CASCADE"); err != nil {
		return fmt.Errorf("data: dropping the schema of %s: %w", plugin, err)
	}
	resourcePrefix := "record://" + plugin + "/"
	for _, statement := range []string{
		"DELETE FROM rookery.refs WHERE starts_with(source, $1) OR starts_with(target, $1)",
		"DELETE FROM rookery.entities WHERE starts_with(uri, $1)",
		"DELETE FROM rookery.changes WHERE starts_with(resource, $1)",
	} {
		if _, err := tx.Exec(ctx, statement, resourcePrefix); err != nil {
			return fmt.Errorf("data: cleaning up derived rows for %s: %w", plugin, err)
		}
	}
	if _, err := tx.Exec(ctx, "DELETE FROM rookery.collections WHERE plugin = $1", plugin); err != nil {
		return fmt.Errorf("data: dropping collection declarations for %s: %w", plugin, err)
	}
	return nil
}

// createCollection makes one collection's table, its indexes and its history
// table.
func createCollection(ctx context.Context, tx pgx.Tx, collection *Collection) error {
	var definition strings.Builder
	definition.WriteString("CREATE TABLE ")
	definition.WriteString(collection.Table())
	definition.WriteString(" (\n\tid text PRIMARY KEY,\n\trev bigint NOT NULL")
	definition.WriteString(",\n\tcreated_at timestamptz NOT NULL DEFAULT now()")
	definition.WriteString(",\n\tupdated_at timestamptz NOT NULL DEFAULT now()")
	definition.WriteString(",\n\tdeleted_at timestamptz")

	if column := collection.ScopeColumn(); column != "" {
		definition.WriteString(",\n\t" + pgIdentifier(column) + " text NOT NULL")
	}
	for _, name := range collection.FieldNames() {
		field := collection.Fields[name]
		definition.WriteString(",\n\t" + pgIdentifier(name) + " " + columnType(field))
		if field.Required {
			definition.WriteString(" NOT NULL")
		}
	}
	definition.WriteString("\n)")

	if _, err := tx.Exec(ctx, definition.String()); err != nil {
		return fmt.Errorf("data: creating the table of %s: %w", collection.Key(), err)
	}

	if column := collection.ScopeColumn(); column != "" {
		index := pgIdentifier(collection.Name + "_" + column + "_idx")
		statement := "CREATE INDEX " + index + " ON " + collection.Table() + " (" + pgIdentifier(column) + ")"
		if _, err := tx.Exec(ctx, statement); err != nil {
			return fmt.Errorf("data: indexing %s on %s: %w", collection.Key(), column, err)
		}
	}

	for _, fields := range collection.Indexes {
		columns := make([]string, len(fields))
		for i, name := range fields {
			columns[i] = pgIdentifier(name)
		}
		index := pgIdentifier(collection.Name + "_" + strings.Join(fields, "_") + "_idx")
		statement := "CREATE INDEX " + index + " ON " + collection.Table() + " (" + strings.Join(columns, ", ") + ")"
		if _, err := tx.Exec(ctx, statement); err != nil {
			return fmt.Errorf("data: indexing %s on %s: %w", collection.Key(), strings.Join(fields, ", "), err)
		}
	}

	if !collection.History {
		return nil
	}
	var history strings.Builder
	history.WriteString("CREATE TABLE ")
	history.WriteString(collection.HistoryTable())
	history.WriteString(` (
	id text NOT NULL,
	rev bigint NOT NULL,
	deleted boolean NOT NULL,
	at timestamptz NOT NULL DEFAULT now(),
	principal text NOT NULL,
	data bytea NOT NULL,
	PRIMARY KEY (id, rev)
)`)
	if _, err := tx.Exec(ctx, history.String()); err != nil {
		return fmt.Errorf("data: creating the history of %s: %w", collection.Key(), err)
	}
	return nil
}

// columnType is the SQL type of a field's column.
func columnType(field Field) string {
	switch field.Type {
	case filter.TypeString, filter.TypeText, filter.TypeEnum, filter.TypeRef:
		return "text"
	case filter.TypeInt:
		return "bigint"
	case filter.TypeFloat:
		return "double precision"
	case filter.TypeBool:
		return "boolean"
	case filter.TypeTime:
		return "timestamptz"
	case filter.TypeCBOR:
		return "bytea"
	case filter.TypeVector:
		// The size is validated as a number, so it cannot carry anything else
		// into the statement.
		return fmt.Sprintf("vector(%d)", field.Size)
	default:
		// Validate refuses an unknown type, so this is unreachable.
		return "text"
	}
}
