package data

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/filter"
	"github.com/aiviarium/rookery/internal/id"
)

// ChangeKind is what a write did.
type ChangeKind string

// What a write did, as the change feed records it.
const (
	ChangeInsert   ChangeKind = "insert"
	ChangeUpdate   ChangeKind = "update"
	ChangeDelete   ChangeKind = "delete"
	ChangeUndelete ChangeKind = "undelete"
)

// Change is one row of the change feed: a write to one record, in the order the
// feed hands them out.
type Change struct {
	// Seq is the change's place in the feed's one global order.
	Seq int64
	// Collection is the record's collection, "plugin/collection".
	Collection string
	// Resource is the record's URI.
	Resource string
	// Kind is what the write did.
	Kind ChangeKind
	// Rev is the record's revision after the write.
	Rev int64
	// SpaceID and OwnerID are the record's scope, when it has one.
	SpaceID string
	OwnerID string
	// Principal is who wrote, as access writes it.
	Principal string
	// Record is the record after the write, and nil for a delete.
	Record map[string]any
	// At is when the write committed.
	At time.Time
}

// Insert writes a record and returns it as it was stored, with its id, revision
// and timestamps.
func (d *Data) Insert(ctx context.Context, principal Principal, collection string, record map[string]any) (map[string]any, error) {
	declared, err := d.Collection(collection)
	if err != nil {
		return nil, err
	}

	var written map[string]any
	err = d.transaction(ctx, func(tx pgx.Tx) error {
		var err error
		written, err = d.insertTx(ctx, tx, principal, declared, record, true)
		return err
	})
	if err != nil {
		return nil, err
	}
	return written, nil
}

// Update changes the fields a record names, leaving the rest, and writes the
// revision the caller expects. A revision of zero means no check; a record at
// another revision is a RevisionError.
func (d *Data) Update(ctx context.Context, principal Principal, collection, id string, record map[string]any, revision int64) (map[string]any, error) {
	declared, err := d.Collection(collection)
	if err != nil {
		return nil, err
	}

	var written map[string]any
	err = d.transaction(ctx, func(tx pgx.Tx) error {
		var err error
		written, err = d.updateTx(ctx, tx, principal, declared, id, record, revision, true)
		return err
	})
	if err != nil {
		return nil, err
	}
	return written, nil
}

// Upsert inserts a record or updates the one its id names.
func (d *Data) Upsert(ctx context.Context, principal Principal, collection string, record map[string]any, revision int64) (map[string]any, error) {
	declared, err := d.Collection(collection)
	if err != nil {
		return nil, err
	}

	var written map[string]any
	err = d.transaction(ctx, func(tx pgx.Tx) error {
		var err error
		written, err = d.upsertTx(ctx, tx, principal, declared, record, revision)
		return err
	})
	if err != nil {
		return nil, err
	}
	return written, nil
}

// Delete marks a record deleted, following every reference's onDelete rule
// first.
func (d *Data) Delete(ctx context.Context, principal Principal, collection, id string, revision int64) error {
	declared, err := d.Collection(collection)
	if err != nil {
		return err
	}
	return d.transaction(ctx, func(tx pgx.Tx) error {
		return d.deleteTx(ctx, tx, principal, declared, id, revision, true, nil)
	})
}

// Restore brings a deleted record back.
func (d *Data) Restore(ctx context.Context, principal Principal, collection, id string, revision int64) (map[string]any, error) {
	declared, err := d.Collection(collection)
	if err != nil {
		return nil, err
	}

	var written map[string]any
	err = d.transaction(ctx, func(tx pgx.Tx) error {
		var err error
		written, err = d.restoreTx(ctx, tx, principal, declared, id, revision)
		return err
	})
	if err != nil {
		return nil, err
	}
	return written, nil
}

// BatchKind is what one operation of a batch does.
type BatchKind string

// What one operation of a batch does.
const (
	BatchInsert  BatchKind = "insert"
	BatchUpdate  BatchKind = "update"
	BatchUpsert  BatchKind = "upsert"
	BatchDelete  BatchKind = "delete"
	BatchRestore BatchKind = "restore"
)

// BatchOp is one operation of a batch.
type BatchOp struct {
	Kind     BatchKind
	ID       string
	Record   map[string]any
	Revision int64
}

// Batch runs operations on one collection in one transaction: either every one
// of them is written or none is. Each operation checks its own access and writes
// its own change row. The results are the records the writes returned, in order;
// a delete has none.
func (d *Data) Batch(ctx context.Context, principal Principal, collection string, operations []BatchOp) ([]map[string]any, error) {
	declared, err := d.Collection(collection)
	if err != nil {
		return nil, err
	}
	if len(operations) == 0 {
		return nil, nil
	}

	records := make([]map[string]any, 0, len(operations))
	err = d.transaction(ctx, func(tx pgx.Tx) error {
		for i, operation := range operations {
			var (
				record map[string]any
				err    error
			)
			switch operation.Kind {
			case BatchInsert:
				record, err = d.insertTx(ctx, tx, principal, declared, operation.Record, true)
			case BatchUpdate:
				record, err = d.updateTx(ctx, tx, principal, declared, operation.ID, operation.Record, operation.Revision, true)
			case BatchUpsert:
				record, err = d.upsertTx(ctx, tx, principal, declared, operation.Record, operation.Revision)
			case BatchDelete:
				err = d.deleteTx(ctx, tx, principal, declared, operation.ID, operation.Revision, true, nil)
			case BatchRestore:
				record, err = d.restoreTx(ctx, tx, principal, declared, operation.ID, operation.Revision)
			default:
				err = fmt.Errorf("%w: %q is not a batch operation", ErrInvalid, operation.Kind)
			}
			if err != nil {
				return fmt.Errorf("data: batch operation %d: %w", i, err)
			}
			if record != nil {
				records = append(records, record)
			}
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	return records, nil
}

// insertTx writes one record in a transaction.
func (d *Data) insertTx(ctx context.Context, tx pgx.Tx, principal Principal, c *Collection, record map[string]any, checkAccess bool) (map[string]any, error) {
	values, err := c.insertValues(record)
	if err != nil {
		return nil, err
	}
	id, err := c.idOf(record, true)
	if err != nil {
		return nil, err
	}
	scopeID, err := c.scopeForInsert(record, principal)
	if err != nil {
		return nil, err
	}
	if checkAccess {
		if err := d.require(ctx, principal, c.resource(scopeID), c.Access.Write); err != nil {
			return nil, err
		}
	}

	names := []string{"id", "rev"}
	args := []any{id, int64(1)}
	expressions := []string{"$1", "$2"}
	if column := c.ScopeColumn(); column != "" {
		names = append(names, column)
		args = append(args, scopeID)
		expressions = append(expressions, fmt.Sprintf("$%d", len(args)))
	}
	for _, name := range c.FieldNames() {
		field := c.Fields[name]
		names = append(names, name)
		args = append(args, columnArg(field, values[name]))
		expression := fmt.Sprintf("$%d", len(args))
		if field.Type == filter.TypeVector && values[name] != nil {
			expression += "::vector"
		}
		expressions = append(expressions, expression)
	}

	quoted := make([]string, len(names))
	for i, name := range names {
		quoted[i] = pgIdentifier(name)
	}
	statement := "INSERT INTO " + c.Table() + " (" + strings.Join(quoted, ", ") + ") VALUES (" +
		strings.Join(expressions, ", ") + ") RETURNING created_at, updated_at"

	var createdAt, updatedAt time.Time
	err = tx.QueryRow(ctx, statement, args...).Scan(&createdAt, &updatedAt)
	if err != nil {
		if isUniqueViolation(err) {
			return nil, fmt.Errorf("%w: the record %s already exists", ErrInvalid, id)
		}
		return nil, fmt.Errorf("data: inserting into %s: %w", c.Key(), err)
	}

	written := c.assemble(id, 1, createdAt.UTC(), updatedAt.UTC(), nil, scopeID, values)
	if err := d.afterWrite(ctx, tx, principal, c, written, ChangeInsert, false); err != nil {
		return nil, err
	}
	return recordMap(written), nil
}

// updateTx changes one record in a transaction. A patch of nil fields clears
// them; fields the patch does not name stay.
func (d *Data) updateTx(ctx context.Context, tx pgx.Tx, principal Principal, c *Collection, id string, patch map[string]any, revision int64, checkAccess bool) (map[string]any, error) {
	values, err := c.patchValues(patch)
	if err != nil {
		return nil, err
	}
	if err := c.refuseScopeChange(patch); err != nil {
		return nil, err
	}

	current, err := d.lockRecord(ctx, tx, c, id)
	if err != nil {
		return nil, err
	}
	if current["deleted_at"] != nil {
		return nil, fmt.Errorf("%w: the record %s of %s is deleted", ErrNotFound, id, c.Key())
	}
	if err := checkRevision(revision, current); err != nil {
		return nil, err
	}
	scopeID := c.scopeOf(current)
	if checkAccess {
		if err := d.require(ctx, principal, c.resource(scopeID), c.Access.Write); err != nil {
			return nil, err
		}
	}

	for name, value := range values {
		current[name] = value
	}
	return d.writeUpdate(ctx, tx, principal, c, current, scopeID)
}

// writeUpdate writes a record already read and patched: the row, then its
// history, its references, its entity and its change.
func (d *Data) writeUpdate(ctx context.Context, tx pgx.Tx, principal Principal, c *Collection, current map[string]any, scopeID string) (map[string]any, error) {
	id := current["id"].(string)
	rev, _ := current["rev"].(int64)
	rev++

	args := []any{id}
	setters := []string{"rev = $2", "updated_at = now()", "deleted_at = NULL"}
	// The revision is the second argument, after the id.
	args = append(args, rev)
	for _, name := range c.FieldNames() {
		field := c.Fields[name]
		args = append(args, columnArg(field, current[name]))
		expression := fmt.Sprintf("$%d", len(args))
		if field.Type == filter.TypeVector && current[name] != nil {
			expression += "::vector"
		}
		setters = append(setters, pgIdentifier(name)+" = "+expression)
	}

	statement := "UPDATE " + c.Table() + " SET " + strings.Join(setters, ", ") +
		" WHERE id = $1 RETURNING updated_at"

	var updatedAt time.Time
	if err := tx.QueryRow(ctx, statement, args...).Scan(&updatedAt); err != nil {
		return nil, fmt.Errorf("data: updating %s: %w", c.Key(), err)
	}

	written := c.assemble(id, rev, current["created_at"].(time.Time), updatedAt.UTC(), nil, scopeID, current)
	if err := d.afterWrite(ctx, tx, principal, c, written, ChangeUpdate, false); err != nil {
		return nil, err
	}
	return recordMap(written), nil
}

// upsertTx inserts a record or updates the one its id names.
func (d *Data) upsertTx(ctx context.Context, tx pgx.Tx, principal Principal, c *Collection, record map[string]any, revision int64) (map[string]any, error) {
	id, err := c.idOf(record, false)
	if err != nil {
		return nil, err
	}
	if id == "" {
		return d.insertTx(ctx, tx, principal, c, record, true)
	}

	current, err := d.lockRecord(ctx, tx, c, id)
	switch {
	case err == nil:
		if current["deleted_at"] != nil {
			return nil, fmt.Errorf("%w: the record %s of %s is deleted", ErrNotFound, id, c.Key())
		}
		// An upsert carries the whole record, so its scope is the one the row
		// has and not a change of it.
		patch := record
		if column := c.ScopeColumn(); column != "" {
			if given, ok := record[column]; ok && given == current[column] {
				patch = make(map[string]any, len(record))
				for name, value := range record {
					if name != column {
						patch[name] = value
					}
				}
			}
		}
		return d.updateTx(ctx, tx, principal, c, id, patch, revision, true)
	case errors.Is(err, ErrNotFound):
		return d.insertTx(ctx, tx, principal, c, record, true)
	default:
		return nil, err
	}
}

// deleteChain is the records one delete is already removing, so that two records
// that cascade into each other end.
type deleteChain struct {
	seen map[string]bool
}

// visit records a record in the chain and reports whether it was new.
func (chain *deleteChain) visit(uri string) bool {
	if chain.seen == nil {
		chain.seen = make(map[string]bool)
	}
	if chain.seen[uri] {
		return false
	}
	chain.seen[uri] = true
	return true
}

// deleteTx marks a record deleted, after following the references that point at
// it.
func (d *Data) deleteTx(ctx context.Context, tx pgx.Tx, principal Principal, c *Collection, id string, revision int64, checkAccess bool, chain *deleteChain) error {
	if chain == nil {
		chain = &deleteChain{}
	}
	if !chain.visit(c.RecordURI(id)) {
		return nil
	}
	current, err := d.lockRecord(ctx, tx, c, id)
	if err != nil {
		return err
	}
	if current["deleted_at"] != nil {
		return fmt.Errorf("%w: the record %s of %s is deleted", ErrNotFound, id, c.Key())
	}
	if err := checkRevision(revision, current); err != nil {
		return err
	}
	scopeID := c.scopeOf(current)
	if checkAccess {
		if err := d.require(ctx, principal, c.resource(scopeID), c.Access.Write); err != nil {
			return err
		}
	}

	if err := d.unlink(ctx, tx, principal, c, current, chain); err != nil {
		return err
	}

	rev, _ := current["rev"].(int64)
	rev++
	var updatedAt time.Time
	statement := "UPDATE " + c.Table() + " SET deleted_at = now(), rev = $1, updated_at = now() WHERE id = $2 RETURNING updated_at, deleted_at"
	var deletedAt time.Time
	err = tx.QueryRow(ctx, statement, rev, id).Scan(&updatedAt, &deletedAt)
	if err != nil {
		return fmt.Errorf("data: deleting from %s: %w", c.Key(), err)
	}

	deleted := c.assemble(id, rev, current["created_at"].(time.Time), updatedAt.UTC(), &deletedAt, scopeID, current)
	if _, err := tx.Exec(ctx, "DELETE FROM rookery.refs WHERE source = $1", c.RecordURI(id)); err != nil {
		return fmt.Errorf("data: dropping the references of %s: %w", c.RecordURI(id), err)
	}
	if _, err := tx.Exec(ctx, "DELETE FROM rookery.entities WHERE uri = $1", c.RecordURI(id)); err != nil {
		return fmt.Errorf("data: dropping the entity of %s: %w", c.RecordURI(id), err)
	}
	return d.afterWrite(ctx, tx, principal, c, deleted, ChangeDelete, true)
}

// restoreTx brings a deleted record back.
func (d *Data) restoreTx(ctx context.Context, tx pgx.Tx, principal Principal, c *Collection, id string, revision int64) (map[string]any, error) {
	current, err := d.lockRecord(ctx, tx, c, id)
	if err != nil {
		return nil, err
	}
	if current["deleted_at"] == nil {
		return nil, fmt.Errorf("%w: the record %s of %s is not deleted", ErrInvalid, id, c.Key())
	}
	if err := checkRevision(revision, current); err != nil {
		return nil, err
	}
	scopeID := c.scopeOf(current)
	if err := d.require(ctx, principal, c.resource(scopeID), c.Access.Write); err != nil {
		return nil, err
	}

	rev, _ := current["rev"].(int64)
	rev++
	var updatedAt time.Time
	statement := "UPDATE " + c.Table() + " SET deleted_at = NULL, rev = $1, updated_at = now() WHERE id = $2 RETURNING updated_at"
	if err := tx.QueryRow(ctx, statement, rev, id).Scan(&updatedAt); err != nil {
		return nil, fmt.Errorf("data: restoring into %s: %w", c.Key(), err)
	}

	restored := c.assemble(id, rev, current["created_at"].(time.Time), updatedAt.UTC(), nil, scopeID, current)
	if err := d.afterWrite(ctx, tx, principal, c, restored, ChangeUndelete, false); err != nil {
		return nil, err
	}
	return recordMap(restored), nil
}

// afterWrite writes what every write owes beside its row: the history, the
// references, the entity and the change.
func (d *Data) afterWrite(ctx context.Context, tx pgx.Tx, principal Principal, c *Collection, record map[string]any, kind ChangeKind, deleted bool) error {
	if err := writeHistory(ctx, tx, principal, c, record, deleted); err != nil {
		return err
	}
	if !deleted {
		if err := writeRefs(ctx, tx, c, record); err != nil {
			return err
		}
		if err := writeEntity(ctx, tx, c, record); err != nil {
			return err
		}
	}
	return writeChange(ctx, tx, principal, c, record, kind, deleted)
}

// unlink applies what deleting a record does to the records that refer to it:
// refuse, clear the field, or delete them.
func (d *Data) unlink(ctx context.Context, tx pgx.Tx, principal Principal, c *Collection, record map[string]any, chain *deleteChain) error {
	id, _ := record["id"].(string)
	uri := c.RecordURI(id)

	rows, err := tx.Query(ctx, "SELECT source, field, on_delete FROM rookery.refs WHERE target = $1 ORDER BY source, field", uri)
	if err != nil {
		return fmt.Errorf("data: reading the references to %s: %w", uri, err)
	}
	type reference struct{ source, field, rule string }
	var references []reference
	for rows.Next() {
		var found reference
		if err := rows.Scan(&found.source, &found.field, &found.rule); err != nil {
			rows.Close()
			return fmt.Errorf("data: reading the references to %s: %w", uri, err)
		}
		references = append(references, found)
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return fmt.Errorf("data: reading the references to %s: %w", uri, err)
	}

	for _, found := range references {
		switch OnDelete(found.rule) {
		case OnDeleteRestrict:
			return &ReferenceError{Source: found.source, Field: found.field}
		case OnDeleteNullify:
			key, sourceID, err := ParseRecordURI(found.source)
			if err != nil {
				return err
			}
			source, err := d.Collection(key)
			if err != nil {
				return &ReferenceError{Source: found.source, Field: found.field}
			}
			current, err := d.lockRecord(ctx, tx, source, sourceID)
			if isNotFound(err) {
				continue
			}
			if err != nil {
				return err
			}
			if current["deleted_at"] != nil {
				continue
			}
			if _, err := d.updateTx(ctx, tx, principal, source, sourceID, map[string]any{found.field: nil}, 0, false); err != nil {
				return fmt.Errorf("data: clearing %s of %s: %w", found.field, found.source, err)
			}
		case OnDeleteCascade:
			key, sourceID, err := ParseRecordURI(found.source)
			if err != nil {
				return err
			}
			source, err := d.Collection(key)
			if err != nil {
				return &ReferenceError{Source: found.source, Field: found.field}
			}
			if err := d.deleteTx(ctx, tx, principal, source, sourceID, 0, false, chain); err != nil && !errors.Is(err, ErrNotFound) {
				return fmt.Errorf("data: deleting %s: %w", found.source, err)
			}
		default:
			return fmt.Errorf("%w: the reference from %s has the rule %q", ErrInvalid, found.source, found.rule)
		}
	}
	return nil
}

// lockRecord reads a record in the transaction, locking it, and returns it with
// its system columns. A record that is not there is pgx.ErrNoRows.
func (d *Data) lockRecord(ctx context.Context, tx pgx.Tx, c *Collection, id string) (map[string]any, error) {
	statement := "SELECT " + c.selectList() + " FROM " + c.Table() + " WHERE id = $1 FOR UPDATE"
	rows, err := tx.Query(ctx, statement, id)
	if err != nil {
		return nil, fmt.Errorf("data: reading %s: %w", c.Key(), err)
	}
	defer rows.Close()
	if !rows.Next() {
		if err := rows.Err(); err != nil {
			return nil, fmt.Errorf("data: reading %s: %w", c.Key(), err)
		}
		return nil, fmt.Errorf("%w: no record %s in %s", ErrNotFound, id, c.Key())
	}
	record, err := c.recordFrom(rows)
	if err != nil {
		return nil, err
	}
	return record, nil
}

// checkRevision compares a write's expected revision with the record's.
func checkRevision(expected int64, record map[string]any) error {
	if expected == 0 {
		return nil
	}
	actual, _ := record["rev"].(int64)
	if expected != actual {
		return &RevisionError{Expected: expected, Actual: actual}
	}
	return nil
}

// scopeOf is a record's scope id: the space's or the owner's, empty for a
// global collection.
func (c *Collection) scopeOf(record map[string]any) string {
	column := c.ScopeColumn()
	if column == "" {
		return ""
	}
	id, _ := record[column].(string)
	return id
}

// idOf is a record's id: the one the caller gave, or a new ULID. A caller may
// leave it out on an insert and may not on an upsert.
func (c *Collection) idOf(record map[string]any, generate bool) (string, error) {
	raw, given := record["id"]
	if !given || raw == nil {
		if generate {
			return id.New(), nil
		}
		return "", nil
	}
	id, ok := raw.(string)
	if !ok || id == "" {
		return "", fmt.Errorf("%w: a record id is a string", ErrInvalid)
	}
	if len(id) > 128 || strings.ContainsAny(id, "/\x00") {
		return "", fmt.Errorf("%w: %q is not a record id", ErrInvalid, id)
	}
	return id, nil
}

// scopeForInsert is the scope id a new record takes: the space or owner the
// caller named, or the caller itself for a user.
func (c *Collection) scopeForInsert(record map[string]any, principal Principal) (string, error) {
	column := c.ScopeColumn()
	if column == "" {
		return "", nil
	}
	raw, given := record[column]
	if given && raw != nil {
		id, ok := raw.(string)
		if !ok || id == "" {
			return "", fmt.Errorf("%w: %s is not a %s id", ErrInvalid, column, column)
		}
		return id, nil
	}
	if c.Scope == ScopeUser {
		if principal.Kind != PrincipalUser {
			return "", fmt.Errorf("%w: a user collection needs %s when a %s writes it", ErrInvalid, column, principal.Kind)
		}
		return principal.ID, nil
	}
	return "", fmt.Errorf("%w: the collection %s is scoped to a %s, so a record needs %s", ErrInvalid, c.Key(), c.Scope, column)
}

// refuseScopeChange stops a patch from moving a record to another space or
// owner, which is not what an update is for.
func (c *Collection) refuseScopeChange(patch map[string]any) error {
	column := c.ScopeColumn()
	if column == "" {
		return nil
	}
	if _, given := patch[column]; given {
		return fmt.Errorf("%w: an update changes no %s", ErrInvalid, column)
	}
	return nil
}

// assemble is a record with its system columns, as it is written, hashed and
// handed out.
func (c *Collection) assemble(id string, rev int64, createdAt, updatedAt time.Time, deletedAt *time.Time, scopeID string, values map[string]any) map[string]any {
	record := make(map[string]any, len(values)+7)
	record["id"] = id
	record["rev"] = rev
	record["created_at"] = createdAt
	record["updated_at"] = updatedAt
	if deletedAt != nil {
		record["deleted_at"] = deletedAt.UTC()
	} else {
		record["deleted_at"] = nil
	}
	if column := c.ScopeColumn(); column != "" {
		record[column] = scopeID
	}
	for name := range c.Fields {
		record[name] = values[name]
	}
	return record
}

// columnArg is a field's value as a statement's argument. A vector goes as its
// literal, which the statement casts.
func columnArg(field Field, value any) any {
	if field.Type != filter.TypeVector {
		return value
	}
	vector, ok := value.([]float32)
	if !ok {
		return nil
	}
	return vectorLiteral(vector)
}

// writeHistory keeps the revision in the collection's history table.
func writeHistory(ctx context.Context, tx pgx.Tx, principal Principal, c *Collection, record map[string]any, deleted bool) error {
	if !c.History {
		return nil
	}
	id, _ := record["id"].(string)
	rev, _ := record["rev"].(int64)
	data, err := cborx.Marshal(recordMap(record))
	if err != nil {
		return fmt.Errorf("data: encoding the history of %s: %w", c.RecordURI(id), err)
	}
	statement := "INSERT INTO " + c.HistoryTable() + " (id, rev, deleted, principal, data) VALUES ($1, $2, $3, $4, $5)"
	if _, err := tx.Exec(ctx, statement, id, rev, deleted, principal.String(), data); err != nil {
		return fmt.Errorf("data: writing the history of %s: %w", c.RecordURI(id), err)
	}
	return nil
}

// writeRefs replaces the reference rows of a record with the ones its fields
// hold.
func writeRefs(ctx context.Context, tx pgx.Tx, c *Collection, record map[string]any) error {
	id, _ := record["id"].(string)
	source := c.RecordURI(id)
	if _, err := tx.Exec(ctx, "DELETE FROM rookery.refs WHERE source = $1", source); err != nil {
		return fmt.Errorf("data: dropping the references of %s: %w", source, err)
	}
	for _, name := range c.FieldNames() {
		field := c.Fields[name]
		if field.Type != filter.TypeRef {
			continue
		}
		target, ok := record[name].(string)
		if !ok || target == "" {
			continue
		}
		rule := field.OnDelete
		if rule == "" {
			rule = OnDeleteRestrict
		}
		statement := "INSERT INTO rookery.refs (source, field, target, on_delete) VALUES ($1, $2, $3, $4)"
		if _, err := tx.Exec(ctx, statement, source, name, target, string(rule)); err != nil {
			return fmt.Errorf("data: writing the reference %s.%s: %w", source, name, err)
		}
	}
	return nil
}

// writeEntity keeps the entity row of a record whose collection declares one.
func writeEntity(ctx context.Context, tx pgx.Tx, c *Collection, record map[string]any) error {
	if c.Entity == nil {
		return nil
	}
	id, _ := record["id"].(string)
	title, _ := record[c.Entity.Title].(string)
	texts := make([]string, 0, len(c.Entity.Text))
	for _, name := range c.Entity.Text {
		if text, ok := record[name].(string); ok && text != "" {
			texts = append(texts, text)
		}
	}

	statement := `INSERT INTO rookery.entities (uri, kind, title, body, access, updated_at)
		VALUES ($1, $2, $3, $4, $5, now())
		ON CONFLICT (uri) DO UPDATE SET
			kind = excluded.kind,
			title = excluded.title,
			body = excluded.body,
			access = excluded.access,
			updated_at = now()`
	access := c.resource(c.scopeOf(record))
	if _, err := tx.Exec(ctx, statement, c.RecordURI(id), c.Entity.Kind, title, strings.Join(texts, " "), access); err != nil {
		return fmt.Errorf("data: writing the entity of %s: %w", c.RecordURI(id), err)
	}
	return nil
}

// writeChange appends the write to the change feed and wakes the feed's loop.
func writeChange(ctx context.Context, tx pgx.Tx, principal Principal, c *Collection, record map[string]any, kind ChangeKind, deleted bool) error {
	id, _ := record["id"].(string)
	rev, _ := record["rev"].(int64)

	var data []byte
	if !deleted {
		encoded, err := cborx.Marshal(recordMap(record))
		if err != nil {
			return fmt.Errorf("data: encoding the change of %s: %w", c.RecordURI(id), err)
		}
		data = encoded
	}

	var spaceID, ownerID *string
	if c.Scope == ScopeSpace {
		value := c.scopeOf(record)
		spaceID = &value
	}
	if c.Scope == ScopeUser {
		value := c.scopeOf(record)
		ownerID = &value
	}

	statement := `INSERT INTO rookery.changes (collection, resource, kind, rev, space_id, owner_id, principal, data)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8)`
	_, err := tx.Exec(ctx, statement, c.Key(), c.RecordURI(id), string(kind), rev, spaceID, ownerID, principal.String(), data)
	if err != nil {
		return fmt.Errorf("data: writing the change of %s: %w", c.RecordURI(id), err)
	}
	if _, err := tx.Exec(ctx, "SELECT pg_notify('rookery_changes', '')"); err != nil {
		return fmt.Errorf("data: notifying the change feed: %w", err)
	}
	return nil
}
