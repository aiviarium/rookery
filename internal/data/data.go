package data

import (
	"context"
	"crypto/rand"
	"encoding/base32"
	"errors"
	"fmt"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgxpool"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/filter"
	"github.com/aiviarium/rookery/internal/store"
)

// Config is what a datastore is opened with.
type Config struct {
	// Store is the database the collections live in.
	Store *store.Store

	// Authorizer answers a principal's level on a resource. It is required:
	// every read and write checks the record's scope.
	Authorizer Authorizer
}

// Data is the datastore: the declarations it knows and the database they are
// in.
type Data struct {
	store *store.Store
	auth  Authorizer

	mu          sync.RWMutex
	collections map[string]*Collection
}

// New returns a datastore with no declarations yet.
func New(config Config) *Data {
	return &Data{
		store:       config.Store,
		auth:        config.Authorizer,
		collections: make(map[string]*Collection),
	}
}

// Store is the database the datastore reads and writes.
func (d *Data) Store() *store.Store { return d.store }

// Declare checks declarations and adds them to the registry. It creates no
// tables: Create does that.
func (d *Data) Declare(collections ...*Collection) error {
	checked := make([]*Collection, 0, len(collections))
	for _, collection := range collections {
		if err := collection.Validate(); err != nil {
			return err
		}
		checked = append(checked, collection)
	}

	d.mu.Lock()
	defer d.mu.Unlock()
	for _, collection := range checked {
		d.collections[collection.Key()] = collection
	}
	return nil
}

// Collection returns a declared collection by "plugin/collection".
func (d *Data) Collection(key string) (*Collection, error) {
	d.mu.RLock()
	defer d.mu.RUnlock()

	collection, ok := d.collections[key]
	if !ok {
		return nil, fmt.Errorf("%w: no collection %q is declared", ErrNotFound, key)
	}
	return collection, nil
}

// Collections are the declarations of one plugin, by name. An empty plugin
// returns every declaration.
func (d *Data) Collections(plugin string) []*Collection {
	d.mu.RLock()
	defer d.mu.RUnlock()

	collections := make([]*Collection, 0, len(d.collections))
	for _, collection := range d.collections {
		if plugin == "" || collection.Plugin == plugin {
			collections = append(collections, collection)
		}
	}
	sort.Slice(collections, func(i, j int) bool { return collections[i].Key() < collections[j].Key() })
	return collections
}

// Declared reports whether a collection is declared.
func (d *Data) Declared(key string) bool {
	d.mu.RLock()
	defer d.mu.RUnlock()
	_, ok := d.collections[key]
	return ok
}

// require checks a principal's level on a resource.
func (d *Data) require(ctx context.Context, principal Principal, resource string, need rookeryv1.Level) error {
	if d.auth == nil {
		return fmt.Errorf("%w: the datastore has no authorizer", ErrForbidden)
	}
	have, err := d.auth.Level(ctx, principal, resource)
	if err != nil {
		return fmt.Errorf("data: checking %s on %s: %w", principal, resource, err)
	}
	if !allows(have, need) {
		return fmt.Errorf("%w: %s has level %d on %s and needs %d", ErrForbidden, principal, have, resource, need)
	}
	return nil
}

// allowed is require for queries that skip a record rather than refuse the call.
func (d *Data) allowed(ctx context.Context, principal Principal, resource string, need rookeryv1.Level) (bool, error) {
	if d.auth == nil {
		return false, fmt.Errorf("%w: the datastore has no authorizer", ErrForbidden)
	}
	have, err := d.auth.Level(ctx, principal, resource)
	if err != nil {
		return false, fmt.Errorf("data: checking %s on %s: %w", principal, resource, err)
	}
	return allows(have, need), nil
}

// allows reports whether a level satisfies a requirement. Admin and local are
// every level; self is the caller's own principal, which manages its own.
func allows(have, need rookeryv1.Level) bool {
	switch have {
	case rookeryv1.Level_LEVEL_ADMIN, rookeryv1.Level_LEVEL_LOCAL:
		return true
	}
	return levelRank(have) >= levelRank(need)
}

// levelRank orders the levels view ⊂ use ⊂ manage. Self counts as manage, since
// an authorizer says self only where the caller owns the resource.
func levelRank(level rookeryv1.Level) int {
	switch level {
	case rookeryv1.Level_LEVEL_VIEW:
		return 1
	case rookeryv1.Level_LEVEL_USE:
		return 2
	case rookeryv1.Level_LEVEL_MANAGE, rookeryv1.Level_LEVEL_SELF:
		return 3
	case rookeryv1.Level_LEVEL_ADMIN, rookeryv1.Level_LEVEL_LOCAL:
		return 4
	default:
		return 0
	}
}

// transaction runs a function in one transaction, rolling it back on an error
// and on a panic.
func (d *Data) transaction(ctx context.Context, run func(tx pgx.Tx) error) error {
	tx, err := d.store.Pool().Begin(ctx)
	if err != nil {
		return fmt.Errorf("data: beginning the transaction: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	if err := run(tx); err != nil {
		return err
	}
	if err := tx.Commit(ctx); err != nil {
		return fmt.Errorf("data: committing the transaction: %w", err)
	}
	return nil
}

// pool is the connection pool, for the queries that need no transaction.
func (d *Data) pool() *pgxpool.Pool { return d.store.Pool() }

// querier is what a transaction and the pool both are, so that reads run in
// either.
type querier interface {
	Query(ctx context.Context, sql string, args ...any) (pgx.Rows, error)
	QueryRow(ctx context.Context, sql string, args ...any) pgx.Row
	Exec(ctx context.Context, sql string, args ...any) (pgconn.CommandTag, error)
}

// selectList is the columns a row is read with, the vector ones as text so that
// no codec is needed on the pool.
func (c *Collection) selectList() string {
	columns := []string{`"id"`, `"rev"`, `"created_at"`, `"updated_at"`, `"deleted_at"`}
	if column := c.ScopeColumn(); column != "" {
		columns = append(columns, pgIdentifier(column))
	}
	for _, name := range c.FieldNames() {
		if c.Fields[name].Type == filter.TypeVector {
			columns = append(columns, pgIdentifier(name)+"::text AS "+pgIdentifier(name))
			continue
		}
		columns = append(columns, pgIdentifier(name))
	}
	return strings.Join(columns, ", ")
}

// recordFrom assembles a record from a row read with selectList.
func (c *Collection) recordFrom(rows pgx.Rows) (map[string]any, error) {
	values, err := rows.Values()
	if err != nil {
		return nil, fmt.Errorf("data: reading a row of %s: %w", c.Key(), err)
	}
	fields := rows.FieldDescriptions()
	if len(fields) != len(values) {
		return nil, fmt.Errorf("data: %s returned %d columns for %d fields", c.Key(), len(values), len(fields))
	}

	record := make(map[string]any, len(values))
	for i, description := range fields {
		value, err := c.columnValue(description.Name, values[i])
		if err != nil {
			return nil, err
		}
		record[description.Name] = value
	}
	return record, nil
}

// recordMap is a record's Go values as the CBOR map a caller gets: the scope's
// column, the timestamps, then the declared fields, all with the types their
// declarations carry.
func recordMap(record map[string]any) map[string]any {
	out := make(map[string]any, len(record))
	for name, value := range record {
		if moment, ok := value.(time.Time); ok {
			out[name] = moment.UTC()
			continue
		}
		out[name] = value
	}
	return out
}

// newID is a ULID: 48 bits of milliseconds and 80 random bits, in Crockford
// base32, so that ids sort by creation time.
func newID() (string, error) {
	var raw [16]byte
	millis := uint64(time.Now().UnixMilli())
	for i := range 6 {
		raw[i] = byte(millis >> (8 * (5 - i)) & 0xff) //nolint:gosec // the mask keeps the byte
	}
	if _, err := rand.Read(raw[6:]); err != nil {
		return "", fmt.Errorf("data: reading an id: %w", err)
	}
	return ulidEncoding.EncodeToString(raw[:]), nil
}

// ulidEncoding is Crockford base32 without the letters ULID leaves out.
var ulidEncoding = base32.NewEncoding("0123456789ABCDEFGHJKMNPQRSTVWXYZ").WithPadding(base32.NoPadding)

// isNotFound reports whether an error is pgx's no-rows error.
func isNotFound(err error) bool { return errors.Is(err, pgx.ErrNoRows) }

// isUniqueViolation reports whether an insert broke a unique constraint, which
// for the datastore is always a record id another write made first.
func isUniqueViolation(err error) bool {
	var pgErr *pgconn.PgError
	return errors.As(err, &pgErr) && pgErr.Code == "23505"
}
