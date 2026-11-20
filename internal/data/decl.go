// Package data is Rookery's datastore.
//
// Plugins declare collections, and the core owns their tables: every write goes
// through this package, which checks access on the record's scope, validates the
// record against the declaration, and keeps the history, the references, the
// entity row and the change row in the same transaction. Reads build their SQL
// with parameters only, from the declaration's fields and the filter package's
// condition.
//
// A collection's table is p_<plugin>.<collection>, with a column per declared
// field beside id, rev, created_at, updated_at, deleted_at and the scope's
// column. A collection with history keeps p_<plugin>.<collection>_history.
//
// The datastore asks an Authorizer for a principal's level on a resource; the
// access package implements it, and this package declares the interface because
// the level is all it needs.
package data

import (
	"context"
	"errors"
	"fmt"
	"regexp"
	"sort"
	"strings"

	"github.com/jackc/pgx/v5"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/filter"
)

// Scope is what a collection's records belong to.
type Scope string

const (
	// ScopeGlobal records belong to the plugin: every caller with the level on
	// plugin://<plugin> sees them.
	ScopeGlobal Scope = "global"

	// ScopeSpace records belong to a space, and the level is on space://<id>.
	ScopeSpace Scope = "space"

	// ScopeUser records belong to a user, and the level is on user://<id>.
	ScopeUser Scope = "user"
)

// OnDelete is what deleting a referenced record does to the record holding the
// reference.
type OnDelete string

const (
	// OnDeleteRestrict refuses the delete.
	OnDeleteRestrict OnDelete = "restrict"

	// OnDeleteNullify clears the holding record's field.
	OnDeleteNullify OnDelete = "nullify"

	// OnDeleteCascade deletes the holding record too.
	OnDeleteCascade OnDelete = "cascade"
)

// The errors the datastore reports. Callers check them with errors.Is; a
// RevisionError and a ReferenceError add detail.
var (
	// ErrInvalid is a declaration or a record that does not follow its
	// declaration.
	ErrInvalid = errors.New("data: invalid")

	// ErrNotFound is a record or a collection that is not there.
	ErrNotFound = errors.New("data: not found")

	// ErrRevision is a write whose expected revision is not the record's.
	ErrRevision = errors.New("data: the revision does not match")

	// ErrReference is a delete that a restriction refuses.
	ErrReference = errors.New("data: a record refers to this one")

	// ErrForbidden is a caller without the level an operation needs.
	ErrForbidden = errors.New("data: forbidden")
)

// RevisionError says which revision a write expected and which it found. It
// wraps ErrRevision.
type RevisionError struct {
	// Expected is the revision the caller gave.
	Expected int64
	// Actual is the record's revision now.
	Actual int64
}

func (e *RevisionError) Error() string {
	return fmt.Sprintf("data: the revision does not match: the write expected %d and the record is at %d", e.Expected, e.Actual)
}

// Unwrap is ErrRevision.
func (e *RevisionError) Unwrap() error { return ErrRevision }

// ReferenceError is a delete that a restriction refuses. It wraps ErrReference.
type ReferenceError struct {
	// Source is the record that refers to the one being deleted.
	Source string
	// Field is the field of that record holding the reference.
	Field string
}

func (e *ReferenceError) Error() string {
	return fmt.Sprintf("data: %s refers to this record in %q, so it cannot be deleted", e.Source, e.Field)
}

// Unwrap is ErrReference.
func (e *ReferenceError) Unwrap() error { return ErrReference }

// PrincipalKind is what a principal is.
type PrincipalKind string

// The kinds a principal has: a person, a plugin's service, and the local admin
// of the server's Unix socket.
const (
	PrincipalUser   PrincipalKind = "user"
	PrincipalPlugin PrincipalKind = "plugin"
	PrincipalLocal  PrincipalKind = "local"
)

// Principal is who a call is for.
type Principal struct {
	Kind PrincipalKind
	// ID is the user's or the plugin's id, empty for the local admin.
	ID string
}

// String is the principal as access writes it: "user:ID", "plugin:ID" or
// "local".
func (p Principal) String() string {
	if p.Kind == PrincipalLocal {
		return "local"
	}
	return string(p.Kind) + ":" + p.ID
}

// UserID is the principal's user id, or empty when it is not a user.
func (p Principal) UserID() string {
	if p.Kind == PrincipalUser {
		return p.ID
	}
	return ""
}

// Authorizer decides a principal's level on a resource. The access package
// implements it.
type Authorizer interface {
	Level(ctx context.Context, principal Principal, resource string) (rookeryv1.Level, error)
}

// Field is one declared field.
type Field struct {
	// Type is the field's type, as the filter language knows it.
	Type filter.Type

	// Size is a vector field's dimensions, and zero for every other type.
	Size int

	// Values are an enum field's values. Empty means any string.
	Values []string

	// Required refuses an insert without the field.
	Required bool

	// Default is the value an insert without the field takes. Validate
	// normalises it to the field's Go type.
	Default any

	// OnDelete is what deleting a referenced record does to this field. It
	// applies to ref fields only, and defaults to restrict.
	OnDelete OnDelete
}

// Entity declares that a collection's records are entities that search and
// resolution find.
type Entity struct {
	// Kind is what the entity is, such as "Note".
	Kind string

	// Title is the field the entity's title comes from.
	Title string

	// Text are the fields search indexes.
	Text []string
}

// Access is the level each operation needs, on the record's scope resource.
type Access struct {
	// Read is the level reading records needs.
	Read rookeryv1.Level

	// Write is the level writing records needs.
	Write rookeryv1.Level
}

// Collection is a plugin's declaration of one collection.
type Collection struct {
	// Plugin is the plugin the collection belongs to.
	Plugin string

	// Name is the collection's name, and its table's.
	Name string

	// Scope is what its records belong to.
	Scope Scope

	// Access is the level each operation needs.
	Access Access

	// History keeps a row per revision in <name>_history.
	History bool

	// Fields are its declared fields, by name.
	Fields map[string]Field

	// Indexes are the field lists the table indexes.
	Indexes [][]string

	// Entity declares the records as entities, for search and resolution.
	Entity *Entity
}

// systemColumns are the columns every collection's table has, which no field
// may be named.
var systemColumns = []string{
	"id", "rev", "created_at", "updated_at", "deleted_at", "space_id", "owner_id",
}

// namePattern is what a plugin or collection name may be: a Postgres identifier
// that needs no quoting beyond what pgx.Identifier does.
var namePattern = regexp.MustCompile(`^[a-z][a-z0-9_]{0,62}$`)

// Validate checks a declaration and normalises its defaults. A declaration that
// is not valid is refused at install, before any table is made.
func (c *Collection) Validate() error {
	switch {
	case !namePattern.MatchString(c.Plugin):
		return fmt.Errorf("%w: %q is not a plugin id", ErrInvalid, c.Plugin)
	case !namePattern.MatchString(c.Name):
		return fmt.Errorf("%w: %q is not a collection name", ErrInvalid, c.Name)
	case strings.HasPrefix(c.Name, "pg_"):
		return fmt.Errorf("%w: %q starts with pg_, which Postgres reserves", ErrInvalid, c.Name)
	case strings.HasSuffix(c.Name, "_history"):
		return fmt.Errorf("%w: %q ends with _history, which a history table takes", ErrInvalid, c.Name)
	}

	switch c.Scope {
	case ScopeGlobal, ScopeSpace, ScopeUser:
	default:
		return fmt.Errorf("%w: %q is not a scope", ErrInvalid, c.Scope)
	}

	if err := checkLevel("read", c.Access.Read); err != nil {
		return err
	}
	if err := checkLevel("write", c.Access.Write); err != nil {
		return err
	}

	for _, name := range systemColumns {
		if _, ok := c.Fields[name]; ok {
			return fmt.Errorf("%w: %q is a column every collection has", ErrInvalid, name)
		}
	}

	for name, field := range c.Fields {
		if !namePattern.MatchString(name) {
			return fmt.Errorf("%w: %q is not a field name", ErrInvalid, name)
		}
		checked, err := checkField(name, field)
		if err != nil {
			return err
		}
		c.Fields[name] = checked
	}

	for _, index := range c.Indexes {
		if len(index) == 0 {
			return fmt.Errorf("%w: an index names no field", ErrInvalid)
		}
		seen := make(map[string]bool, len(index))
		for _, name := range index {
			field, ok := c.Fields[name]
			if !ok {
				return fmt.Errorf("%w: an index names the field %q, which is not declared", ErrInvalid, name)
			}
			switch field.Type {
			case filter.TypeVector, filter.TypeCBOR:
				return fmt.Errorf("%w: %q cannot be indexed: a %s field takes no index", ErrInvalid, name, field.Type)
			}
			if seen[name] {
				return fmt.Errorf("%w: an index names %q twice", ErrInvalid, name)
			}
			seen[name] = true
		}
	}

	if c.Entity != nil {
		return c.validateEntity()
	}

	// A ref field's onDelete is restrict unless it says otherwise.
	for name, field := range c.Fields {
		if field.Type == filter.TypeRef && field.OnDelete == "" {
			field.OnDelete = OnDeleteRestrict
			c.Fields[name] = field
		}
	}
	return nil
}

// checkField validates one field and normalises its default.
func checkField(name string, field Field) (Field, error) {
	switch field.Type {
	case filter.TypeVector:
		if field.Size < 1 || field.Size > 16000 {
			return field, fmt.Errorf("%w: the vector field %q needs a size from 1 to 16000", ErrInvalid, name)
		}
	case filter.TypeString, filter.TypeText, filter.TypeInt, filter.TypeFloat,
		filter.TypeBool, filter.TypeTime, filter.TypeEnum, filter.TypeRef, filter.TypeCBOR:
		if field.Size != 0 {
			return field, fmt.Errorf("%w: the field %q takes no size", ErrInvalid, name)
		}
	default:
		return field, fmt.Errorf("%w: the field %q has no type", ErrInvalid, name)
	}

	if field.Type == filter.TypeEnum && len(field.Values) > 0 {
		seen := make(map[string]bool, len(field.Values))
		for _, value := range field.Values {
			if value == "" {
				return field, fmt.Errorf("%w: the enum field %q has an empty value", ErrInvalid, name)
			}
			if seen[value] {
				return field, fmt.Errorf("%w: the enum field %q has %q twice", ErrInvalid, name, value)
			}
			seen[value] = true
		}
	}

	switch field.OnDelete {
	case "":
		field.OnDelete = OnDeleteRestrict
	case OnDeleteRestrict, OnDeleteNullify, OnDeleteCascade:
	default:
		return field, fmt.Errorf("%w: %q is not an onDelete rule", ErrInvalid, field.OnDelete)
	}
	if field.Type != filter.TypeRef && field.OnDelete != OnDeleteRestrict {
		return field, fmt.Errorf("%w: onDelete applies to ref fields, not %q", ErrInvalid, name)
	}
	if field.OnDelete == OnDeleteNullify && field.Required {
		return field, fmt.Errorf("%w: the required field %q cannot be nullified", ErrInvalid, name)
	}

	if field.Default != nil {
		value, err := (fieldType{field: field}).normalise(field.Default)
		if err != nil {
			return field, fmt.Errorf("%w: the default of %q: %w", ErrInvalid, name, err)
		}
		if field.Required && field.Type == filter.TypeRef && value == nil {
			return field, fmt.Errorf("%w: the default of %q is null", ErrInvalid, name)
		}
		field.Default = value
	}

	if field.Required && field.Type == filter.TypeVector {
		return field, fmt.Errorf("%w: the vector field %q cannot be required", ErrInvalid, name)
	}
	return field, nil
}

// validateEntity checks the entity block.
func (c *Collection) validateEntity() error {
	entity := c.Entity
	if entity.Kind == "" {
		return fmt.Errorf("%w: an entity needs a kind", ErrInvalid)
	}
	title, ok := c.Fields[entity.Title]
	if !ok {
		return fmt.Errorf("%w: the entity's title field %q is not declared", ErrInvalid, entity.Title)
	}
	if title.Type != filter.TypeText && title.Type != filter.TypeString {
		return fmt.Errorf("%w: the entity's title field %q is not text", ErrInvalid, entity.Title)
	}
	if len(entity.Text) == 0 {
		return fmt.Errorf("%w: an entity needs at least one text field", ErrInvalid)
	}
	for _, name := range entity.Text {
		field, ok := c.Fields[name]
		if !ok {
			return fmt.Errorf("%w: the entity's text field %q is not declared", ErrInvalid, name)
		}
		if field.Type != filter.TypeText && field.Type != filter.TypeString {
			return fmt.Errorf("%w: the entity's text field %q is not text", ErrInvalid, name)
		}
	}
	return nil
}

// checkLevel refuses a level an access check cannot mean.
func checkLevel(operation string, level rookeryv1.Level) error {
	switch level {
	case rookeryv1.Level_LEVEL_VIEW, rookeryv1.Level_LEVEL_USE, rookeryv1.Level_LEVEL_MANAGE,
		rookeryv1.Level_LEVEL_ADMIN, rookeryv1.Level_LEVEL_SELF, rookeryv1.Level_LEVEL_LOCAL:
		return nil
	default:
		return fmt.Errorf("%w: the %s level is not set", ErrInvalid, operation)
	}
}

// Key is the collection's name in the datastore's registry: "plugin/collection".
func (c *Collection) Key() string { return c.Plugin + "/" + c.Name }

// Schema is the Postgres schema the collection's table is in.
func (c *Collection) Schema() string { return "p_" + c.Plugin }

// Table is the collection's table, qualified and quoted.
func (c *Collection) Table() string {
	return pgIdentifier(c.Schema(), c.Name)
}

// HistoryTable is the collection's history table, qualified and quoted, and
// empty when the collection keeps no history.
func (c *Collection) HistoryTable() string {
	if !c.History {
		return ""
	}
	return pgIdentifier(c.Schema(), c.Name+"_history")
}

// FieldNames are the declared fields' names, sorted, which is the order the
// datastore writes them in.
func (c *Collection) FieldNames() []string {
	names := make([]string, 0, len(c.Fields))
	for name := range c.Fields {
		names = append(names, name)
	}
	sort.Strings(names)
	return names
}

// ScopeColumn is the column the scope's id is kept in, and empty for a global
// collection.
func (c *Collection) ScopeColumn() string {
	switch c.Scope {
	case ScopeSpace:
		return "space_id"
	case ScopeUser:
		return "owner_id"
	default:
		return ""
	}
}

// FilterFields are the fields a filter may name.
func (c *Collection) FilterFields() filter.Fields {
	fields := make(filter.Fields, len(c.Fields))
	for name, field := range c.Fields {
		fields[name] = field.Type
	}
	return fields
}

// RecordURI is a record's URI, which references and entities use:
// record://<plugin>/<collection>/<id>.
func (c *Collection) RecordURI(id string) string {
	return "record://" + c.Plugin + "/" + c.Name + "/" + id
}

// ParseRecordURI splits a record URI into the collection's key and the id.
func ParseRecordURI(uri string) (key, id string, err error) {
	rest, ok := strings.CutPrefix(uri, "record://")
	if !ok {
		return "", "", fmt.Errorf("%w: %q is not a record URI", ErrInvalid, uri)
	}
	parts := strings.SplitN(rest, "/", 3)
	if len(parts) != 3 || parts[0] == "" || parts[1] == "" || parts[2] == "" {
		return "", "", fmt.Errorf("%w: %q is not a record URI", ErrInvalid, uri)
	}
	return parts[0] + "/" + parts[1], parts[2], nil
}

// resource is the resource URI the record's access is checked on: the space, the
// owner or the plugin.
func (c *Collection) resource(id string) string {
	switch c.Scope {
	case ScopeSpace:
		return "space://" + id
	case ScopeUser:
		return "user://" + id
	default:
		return "plugin://" + c.Plugin
	}
}

// pgIdentifier quotes identifiers for a statement.
func pgIdentifier(parts ...string) string {
	identifier := pgx.Identifier{}
	for _, part := range parts {
		identifier = append(identifier, part)
	}
	return identifier.Sanitize()
}
