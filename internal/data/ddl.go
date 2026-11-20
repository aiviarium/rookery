package data

import (
	"context"
	"fmt"
	"strings"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/filter"
)

// Create makes a plugin's schema and the tables its collections need. The
// plugin system runs it at install, in one transaction with the rest of the
// plugin's rows, so a failure leaves nothing behind.
//
// A collection's table is dropped and made again only by a reset: declarations
// that differ from the installed ones are refused before this is called.
func (d *Data) Create(ctx context.Context, plugin string) error {
	collections := d.Collections(plugin)
	if len(collections) == 0 {
		return nil
	}

	return d.transaction(ctx, func(tx pgx.Tx) error {
		schema := pgIdentifier("p_" + plugin)
		if _, err := tx.Exec(ctx, "CREATE SCHEMA IF NOT EXISTS "+schema); err != nil {
			return fmt.Errorf("data: creating the schema of %s: %w", plugin, err)
		}
		for _, collection := range collections {
			if err := createCollection(ctx, tx, collection); err != nil {
				return err
			}
		}
		return nil
	})
}

// Drop removes a plugin's schema and every table in it. The plugin system runs
// it on a reset, or when a plugin is removed with its data.
func (d *Data) Drop(ctx context.Context, plugin string) error {
	_, err := d.pool().Exec(ctx, "DROP SCHEMA IF EXISTS "+pgIdentifier("p_"+plugin)+" CASCADE")
	if err != nil {
		return fmt.Errorf("data: dropping the schema of %s: %w", plugin, err)
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
