package data

import (
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"time"

	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/filter"
	"github.com/jackc/pgx/v5"
)

// Sort orders the records a query returns.
type Sort struct {
	// Field is a declared field's name.
	Field string
	// Desc is true for the largest first.
	Desc bool
}

// Query is what a Find reads: a filter, an order, a window and the caller's
// current space.
type Query struct {
	// Filter selects records. Nil matches every record.
	Filter *filter.Filter
	// Sorts order them. Empty orders by id.
	Sorts []Sort
	// Limit is how many records to return. Zero is the default, 100; 1000 is
	// the most.
	Limit int
	// Cursor continues a previous Find.
	Cursor string
	// Space is the caller's current space. A space-scoped collection reads
	// one space's records, so it is required there.
	Space string
}

// Page is a Find's records and the cursor for the next page.
type Page struct {
	Records []map[string]any
	// Cursor continues the query after these records, and is empty when there
	// are no more.
	Cursor string
}

// The window's bounds.
const (
	defaultLimit = 100
	maxLimit     = 1000
)

// Get reads one record.
func (d *Data) Get(ctx context.Context, principal Principal, collection, id string) (map[string]any, error) {
	c, err := d.Collection(collection)
	if err != nil {
		return nil, err
	}
	record, err := d.loadRecord(ctx, d.pool(), c, id)
	if err != nil {
		return nil, err
	}
	if record["deleted_at"] != nil {
		return nil, fmt.Errorf("%w: the record %s of %s is deleted", ErrNotFound, id, c.Key())
	}
	if err := d.require(ctx, principal, c.resource(c.scopeOf(record)), c.Access.Read); err != nil {
		return nil, err
	}
	return recordMap(record), nil
}

// Find reads the records a query selects, in pages.
func (d *Data) Find(ctx context.Context, principal Principal, collection string, query Query) (Page, error) {
	c, err := d.Collection(collection)
	if err != nil {
		return Page{}, err
	}
	where, args, err := d.scopeClause(ctx, principal, c, query.Space)
	if err != nil {
		return Page{}, err
	}
	return d.find(ctx, d.pool(), c, query, where, args)
}

func (d *Data) find(ctx context.Context, q querier, c *Collection, query Query, where []string, args []any) (Page, error) {
	if err := query.Filter.Validate(c.FilterFields()); err != nil {
		return Page{}, err
	}
	if err := c.checkSorts(query.Sorts); err != nil {
		return Page{}, err
	}
	sorts := withID(query.Sorts)
	where = append(where, "deleted_at IS NULL")
	if query.Filter != nil {
		clause, filterArgs, err := query.Filter.SQL(len(args) + 1)
		if err != nil {
			return Page{}, err
		}
		where = append(where, clause)
		args = append(args, filterArgs...)
	}

	limit := clampLimit(query.Limit)
	order := orderBy(sorts)

	statement := "SELECT " + c.selectList() + " FROM " + c.Table() + " WHERE " +
		strings.Join(where, " AND ")
	statementArgs := args
	if query.Cursor != "" {
		condition, cursorArgs, err := c.cursorCondition(sorts, query.Cursor, len(args)+1)
		if err != nil {
			return Page{}, err
		}
		statement += " AND " + condition
		statementArgs = append(append([]any{}, args...), cursorArgs...)
	}
	statement += " ORDER BY " + order + " LIMIT " + strconv.Itoa(limit+1)

	rows, err := q.Query(ctx, statement, statementArgs...)
	if err != nil {
		return Page{}, fmt.Errorf("data: finding in %s: %w", c.Key(), err)
	}
	defer rows.Close()

	records := make([]map[string]any, 0, limit+1)
	for rows.Next() {
		record, err := c.recordFrom(rows)
		if err != nil {
			return Page{}, err
		}
		records = append(records, record)
	}
	if err := rows.Err(); err != nil {
		return Page{}, fmt.Errorf("data: finding in %s: %w", c.Key(), err)
	}

	page := Page{Records: make([]map[string]any, 0, len(records))}
	if len(records) > limit {
		records = records[:limit]
		page.Cursor, err = c.encodeCursor(sorts, records[len(records)-1])
		if err != nil {
			return Page{}, err
		}
	}
	for _, record := range records {
		page.Records = append(page.Records, recordMap(record))
	}
	return page, nil
}

// Count counts the records a query selects.
func (d *Data) Count(ctx context.Context, principal Principal, collection string, query Query) (int64, error) {
	c, err := d.Collection(collection)
	if err != nil {
		return 0, err
	}
	if err := query.Filter.Validate(c.FilterFields()); err != nil {
		return 0, err
	}
	where, args, err := d.scopeClause(ctx, principal, c, query.Space)
	if err != nil {
		return 0, err
	}
	where = append(where, "deleted_at IS NULL")
	if query.Filter != nil {
		clause, filterArgs, err := query.Filter.SQL(len(args) + 1)
		if err != nil {
			return 0, err
		}
		where = append(where, clause)
		args = append(args, filterArgs...)
	}

	statement := "SELECT count(*) FROM " + c.Table() + " WHERE " + strings.Join(where, " AND ")
	var count int64
	if err := d.pool().QueryRow(ctx, statement, args...).Scan(&count); err != nil {
		return 0, fmt.Errorf("data: counting in %s: %w", c.Key(), err)
	}
	return count, nil
}

// AggregateOp is what an aggregate computes per group.
type AggregateOp string

// The operations an aggregate computes.
const (
	AggregateCount AggregateOp = "count"
	AggregateSum   AggregateOp = "sum"
	AggregateMin   AggregateOp = "min"
	AggregateMax   AggregateOp = "max"
	AggregateAvg   AggregateOp = "avg"
)

// Aggregate is what an Aggregate reads.
type Aggregate struct {
	// Op is what to compute.
	Op AggregateOp
	// Field is the field to compute it on, and empty for count.
	Field string
	// GroupBy are the fields to group by.
	GroupBy []string
	// Filter selects the records. Nil matches every record.
	Filter *filter.Filter
	// Space is the caller's current space, as a Query's.
	Space string
}

// AggregateRow is one group's value.
type AggregateRow struct {
	// Group holds each grouping field's value.
	Group map[string]any
	// Value is the aggregate: an int64 for count, a float64 for sum and avg,
	// and the field's own type for min and max.
	Value any
}

// Aggregate computes an aggregate over the records a query selects.
func (d *Data) Aggregate(ctx context.Context, principal Principal, collection string, aggregate Aggregate) ([]AggregateRow, error) {
	c, err := d.Collection(collection)
	if err != nil {
		return nil, err
	}
	if err := aggregate.Filter.Validate(c.FilterFields()); err != nil {
		return nil, err
	}

	value, err := aggregateExpression(c, aggregate)
	if err != nil {
		return nil, err
	}
	where, args, err := d.scopeClause(ctx, principal, c, aggregate.Space)
	if err != nil {
		return nil, err
	}
	where = append(where, "deleted_at IS NULL")
	if aggregate.Filter != nil {
		clause, filterArgs, err := aggregate.Filter.SQL(len(args) + 1)
		if err != nil {
			return nil, err
		}
		where = append(where, clause)
		args = append(args, filterArgs...)
	}

	columns := make([]string, len(aggregate.GroupBy))
	for i, name := range aggregate.GroupBy {
		columns[i] = pgIdentifier(name)
	}
	statement := "SELECT " + strings.Join(append(append([]string{}, columns...), value), ", ") +
		" FROM " + c.Table() + " WHERE " + strings.Join(where, " AND ")
	if len(columns) > 0 {
		statement += " GROUP BY " + strings.Join(columns, ", ") + " ORDER BY " + strings.Join(columns, ", ")
	}

	rows, err := d.pool().Query(ctx, statement, args...)
	if err != nil {
		return nil, fmt.Errorf("data: aggregating %s: %w", c.Key(), err)
	}
	defer rows.Close()

	results := make([]AggregateRow, 0, 8)
	for rows.Next() {
		values, err := rows.Values()
		if err != nil {
			return nil, fmt.Errorf("data: aggregating %s: %w", c.Key(), err)
		}
		row := AggregateRow{Group: make(map[string]any, len(columns))}
		for i, name := range aggregate.GroupBy {
			value, err := c.columnValue(name, values[i])
			if err != nil {
				return nil, err
			}
			row.Group[name] = value
		}
		row.Value = normaliseAggregate(values[len(values)-1])
		results = append(results, row)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("data: aggregating %s: %w", c.Key(), err)
	}
	return results, nil
}

// aggregateExpression validates an aggregate and returns the SQL that computes
// it.
func aggregateExpression(c *Collection, aggregate Aggregate) (string, error) {
	switch aggregate.Op {
	case AggregateCount:
		if aggregate.Field == "" {
			return "count(*)", nil
		}
		if _, ok := c.Fields[aggregate.Field]; !ok {
			return "", fmt.Errorf("%w: the collection %s declares no field %q", ErrInvalid, c.Key(), aggregate.Field)
		}
		return "count(" + pgIdentifier(aggregate.Field) + ")", nil
	case AggregateSum, AggregateAvg:
		field, ok := c.Fields[aggregate.Field]
		if !ok {
			return "", fmt.Errorf("%w: the collection %s declares no field %q", ErrInvalid, c.Key(), aggregate.Field)
		}
		if field.Type != filter.TypeInt && field.Type != filter.TypeFloat {
			return "", fmt.Errorf("%w: %s takes a number, not a %s field", ErrInvalid, aggregate.Op, field.Type)
		}
		return string(aggregate.Op) + "(" + pgIdentifier(aggregate.Field) + ")::double precision", nil
	case AggregateMin, AggregateMax:
		field, ok := c.Fields[aggregate.Field]
		if !ok {
			return "", fmt.Errorf("%w: the collection %s declares no field %q", ErrInvalid, c.Key(), aggregate.Field)
		}
		switch field.Type {
		case filter.TypeInt, filter.TypeFloat, filter.TypeTime, filter.TypeString, filter.TypeText, filter.TypeEnum, filter.TypeRef:
		default:
			return "", fmt.Errorf("%w: %s takes no %s field", ErrInvalid, aggregate.Op, field.Type)
		}
		return string(aggregate.Op) + "(" + pgIdentifier(aggregate.Field) + ")", nil
	default:
		return "", fmt.Errorf("%w: %q is not an aggregate", ErrInvalid, aggregate.Op)
	}
}

// normaliseAggregate turns a scanned aggregate value into the one a caller gets.
func normaliseAggregate(value any) any {
	switch number := value.(type) {
	case time.Time:
		return number.UTC()
	case int32:
		return int64(number)
	case int64, float64, string, bool, nil:
		return number
	default:
		return fmt.Sprint(number)
	}
}

// History returns a record's revisions, oldest first.
func (d *Data) History(ctx context.Context, principal Principal, collection, id string, fromRevision int64, limit int) ([]map[string]any, error) {
	c, err := d.Collection(collection)
	if err != nil {
		return nil, err
	}
	if !c.History {
		return nil, fmt.Errorf("%w: the collection %s keeps no history", ErrNotFound, c.Key())
	}
	current, err := d.loadRecord(ctx, d.pool(), c, id)
	if err != nil {
		return nil, err
	}
	if err := d.require(ctx, principal, c.resource(c.scopeOf(current)), c.Access.Read); err != nil {
		return nil, err
	}
	if limit <= 0 || limit > maxLimit {
		limit = defaultLimit
	}

	statement := "SELECT rev, deleted, at, principal, data FROM " + c.HistoryTable() +
		" WHERE id = $1 AND rev > $2 ORDER BY rev LIMIT $3"
	rows, err := d.pool().Query(ctx, statement, id, fromRevision, limit)
	if err != nil {
		return nil, fmt.Errorf("data: reading the history of %s: %w", c.RecordURI(id), err)
	}
	defer rows.Close()

	entries := make([]map[string]any, 0, limit)
	for rows.Next() {
		var (
			rev       int64
			deleted   bool
			at        time.Time
			principal string
			data      []byte
		)
		if err := rows.Scan(&rev, &deleted, &at, &principal, &data); err != nil {
			return nil, fmt.Errorf("data: reading the history of %s: %w", c.RecordURI(id), err)
		}
		record := map[string]any{}
		if len(data) > 0 {
			if err := cborx.Unmarshal(data, &record); err != nil {
				return nil, fmt.Errorf("data: decoding the history of %s: %w", c.RecordURI(id), err)
			}
		}
		entries = append(entries, map[string]any{
			"id":        id,
			"rev":       rev,
			"deleted":   deleted,
			"at":        at.UTC(),
			"principal": principal,
			"record":    record,
		})
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("data: reading the history of %s: %w", c.RecordURI(id), err)
	}
	return entries, nil
}

// Ref is one row of the reference table.
type Ref struct {
	// Source is the record holding the reference, as a URI.
	Source string
	// Field is the field it is held in.
	Field string
	// Target is the resource the field names.
	Target string
	// OnDelete is what deleting the target does.
	OnDelete OnDelete
}

// RefsTo lists the references that point at a resource, skipping the records the
// principal may not read.
func (d *Data) RefsTo(ctx context.Context, principal Principal, target string) ([]Ref, error) {
	return d.refs(ctx, principal, target, true)
}

// RefsFrom lists the references a record holds, skipping unreadable targets.
func (d *Data) RefsFrom(ctx context.Context, principal Principal, source string) ([]Ref, error) {
	if _, _, err := ParseRecordURI(source); err != nil {
		return nil, err
	}
	return d.refs(ctx, principal, source, false)
}

func (d *Data) refs(ctx context.Context, principal Principal, uri string, inbound bool) ([]Ref, error) {
	allowed, err := d.canRead(ctx, principal, uri)
	if err != nil {
		return nil, err
	}
	if !allowed {
		return nil, fmt.Errorf("%w: may not read %s", ErrForbidden, uri)
	}
	statement := "SELECT source, field, target, on_delete FROM rookery.refs WHERE source = $1 ORDER BY field, target"
	if inbound {
		statement = "SELECT source, field, target, on_delete FROM rookery.refs WHERE target = $1 ORDER BY source, field"
	}
	rows, err := d.pool().Query(ctx, statement, uri)
	if err != nil {
		return nil, fmt.Errorf("data: reading references: %w", err)
	}
	refs, err := pgx.CollectRows(rows, func(row pgx.CollectableRow) (Ref, error) {
		var ref Ref
		err := row.Scan(&ref.Source, &ref.Field, &ref.Target, &ref.OnDelete)
		return ref, err
	})
	if err != nil {
		return nil, fmt.Errorf("data: reading references: %w", err)
	}
	visible := refs[:0]
	for _, ref := range refs {
		other := ref.Target
		if inbound {
			other = ref.Source
		}
		allowed, err := d.canRead(ctx, principal, other)
		if err != nil {
			return nil, err
		}
		if allowed {
			visible = append(visible, ref)
		}
	}
	return visible, nil
}

// canRead checks a record at its declared level and other resources at view.
func (d *Data) canRead(ctx context.Context, principal Principal, uri string) (bool, error) {
	key, id, err := ParseRecordURI(uri)
	if err != nil {
		return d.allowed(ctx, principal, uri, cReadLevel)
	}
	c, err := d.Collection(key)
	if errors.Is(err, ErrNotFound) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	record, err := d.loadRecord(ctx, d.pool(), c, id)
	if errors.Is(err, ErrNotFound) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	if record["deleted_at"] != nil {
		return false, nil
	}
	return d.allowed(ctx, principal, c.resource(c.scopeOf(record)), c.Access.Read)
}

// loadRecord reads a record without locking it, deleted or not.
func (d *Data) loadRecord(ctx context.Context, q querier, c *Collection, id string) (map[string]any, error) {
	statement := "SELECT " + c.selectList() + " FROM " + c.Table() + " WHERE id = $1"
	rows, err := q.Query(ctx, statement, id)
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
	return c.recordFrom(rows)
}

// scopeClause is the condition that limits a query to what the principal may
// read on the collection's scope, with its argument.
func (d *Data) scopeClause(ctx context.Context, principal Principal, c *Collection, space string) ([]string, []any, error) {
	switch c.Scope {
	case ScopeGlobal:
		if err := d.require(ctx, principal, c.resource(""), c.Access.Read); err != nil {
			return nil, nil, err
		}
		return nil, nil, nil
	case ScopeSpace:
		if space == "" {
			return nil, nil, fmt.Errorf("%w: the collection %s is scoped to spaces, so a space is needed", ErrInvalid, c.Key())
		}
		if err := d.require(ctx, principal, c.resource(space), c.Access.Read); err != nil {
			return nil, nil, err
		}
		args := []any{space}
		return []string{"space_id = $1"}, args, nil
	case ScopeUser:
		if principal.Kind != PrincipalUser || principal.ID == "" {
			return nil, nil, fmt.Errorf("%w: the collection %s is scoped to users, so a user reads it", ErrForbidden, c.Key())
		}
		if err := d.require(ctx, principal, c.resource(principal.ID), c.Access.Read); err != nil {
			return nil, nil, err
		}
		args := []any{principal.ID}
		return []string{"owner_id = $1"}, args, nil
	default:
		return nil, nil, fmt.Errorf("%w: %q is not a scope", ErrInvalid, c.Scope)
	}
}

// checkSorts refuses an order on a field that has none.
func (c *Collection) checkSorts(sorts []Sort) error {
	for _, sort := range sorts {
		field, ok := c.Fields[sort.Field]
		if !ok {
			return fmt.Errorf("%w: the collection %s declares no field %q", ErrInvalid, c.Key(), sort.Field)
		}
		switch field.Type {
		case filter.TypeVector, filter.TypeCBOR:
			return fmt.Errorf("%w: a %s field cannot be ordered on", ErrInvalid, field.Type)
		}
	}
	return nil
}

// withID appends the id to an order, so that a page is always deterministic.
func withID(sorts []Sort) []Sort {
	for _, sort := range sorts {
		if sort.Field == "id" {
			return sorts
		}
	}
	return append(append([]Sort{}, sorts...), Sort{Field: "id"})
}

// orderBy is an order as SQL.
func orderBy(sorts []Sort) string {
	parts := make([]string, len(sorts))
	for i, sort := range sorts {
		direction := "ASC"
		if sort.Desc {
			direction = "DESC"
		}
		parts[i] = pgIdentifier(sort.Field) + " " + direction + " NULLS LAST"
	}
	return strings.Join(parts, ", ")
}

// clampLimit bounds a page's size.
func clampLimit(limit int) int {
	switch {
	case limit <= 0:
		return defaultLimit
	case limit > maxLimit:
		return maxLimit
	default:
		return limit
	}
}

// cursorCondition is the condition that skips the records a cursor covered: the
// rows after it in the query's order, nulls last.
func (c *Collection) cursorCondition(sorts []Sort, cursor string, first int) (string, []any, error) {
	values, err := c.decodeCursor(sorts, cursor)
	if err != nil {
		return "", nil, err
	}

	args := make([]any, 0, len(sorts))
	prefix := make([]string, 0, len(sorts))
	parts := make([]string, 0, len(sorts))
	for i, sort := range sorts {
		placeholder := fmt.Sprintf("$%d", first+len(args))
		args = append(args, values[i])
		column := pgIdentifier(sort.Field)

		after := "false"
		if values[i] != nil {
			operator := ">"
			if sort.Desc {
				operator = "<"
			}
			after = "(" + column + " " + operator + " " + placeholder + " OR " + column + " IS NULL)"
		}
		parts = append(parts, "("+strings.Join(append(append([]string{}, prefix...), after), " AND ")+")")
		prefix = append(prefix, column+" IS NOT DISTINCT FROM "+placeholder)
	}
	return "(" + strings.Join(parts, " OR ") + ")", args, nil
}

// encodeCursor is the cursor after a record.
func (c *Collection) encodeCursor(sorts []Sort, record map[string]any) (string, error) {
	values := make([]any, 0, len(sorts))
	for _, sort := range sorts {
		values = append(values, record[sort.Field])
	}
	encoded, err := cborx.Marshal(values)
	if err != nil {
		return "", fmt.Errorf("data: encoding a cursor: %w", err)
	}
	return base64.RawURLEncoding.EncodeToString(encoded), nil
}

// decodeCursor reads the values a cursor covers, in the order's fields.
func (c *Collection) decodeCursor(sorts []Sort, cursor string) ([]any, error) {
	raw, err := base64.RawURLEncoding.DecodeString(cursor)
	if err != nil {
		return nil, fmt.Errorf("%w: the cursor is not base64: %w", ErrInvalid, err)
	}
	var values []any
	if err := cborx.Unmarshal(raw, &values); err != nil {
		return nil, fmt.Errorf("%w: the cursor does not decode: %w", ErrInvalid, err)
	}
	if len(values) != len(sorts) {
		return nil, fmt.Errorf("%w: the cursor holds %d values for %d fields", ErrInvalid, len(values), len(sorts))
	}
	for i, sort := range sorts {
		field, ok := c.Fields[sort.Field]
		if !ok {
			// The id.
			if _, ok := values[i].(string); !ok {
				return nil, fmt.Errorf("%w: the cursor's id is not a string", ErrInvalid)
			}
			continue
		}
		value, err := (fieldType{field: field}).normalise(values[i])
		if err != nil {
			return nil, fmt.Errorf("%w: the cursor's %s: %w", ErrInvalid, sort.Field, err)
		}
		values[i] = value
	}
	return values, nil
}
