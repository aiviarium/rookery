// The differential test: random filters are run through Postgres and through
// the in-memory matcher over the same rows, and the two have to choose the same
// ones. It is why the filter language has one implementation of its meaning, and
// why a new operator cannot land in one without the other.
package filter_test

import (
	"fmt"
	"math/rand/v2"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/filter"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// diffFields are the collection's fields, one per filterable type.
var diffFields = filter.Fields{
	"title": filter.TypeText,
	"name":  filter.TypeString,
	"state": filter.TypeEnum,
	"about": filter.TypeRef,
	"done":  filter.TypeBool,
	"count": filter.TypeInt,
	"score": filter.TypeFloat,
	"at":    filter.TypeTime,
	"body":  filter.TypeCBOR,
}

// diffTable is the table the rows live in, with a column per field.
const diffTable = `
CREATE TABLE diff_records (
	id    bigint PRIMARY KEY,
	title text,
	name  text,
	state text,
	about text,
	done  boolean,
	count bigint,
	score double precision,
	at    timestamptz,
	body  bytea
)`

// diffRows is how many rows the test writes.
const diffRows = 48

// diffRecord is one row, as the matcher sees it: the declared Go type per field,
// nil for null.
type diffRecord struct {
	id     int64
	record map[string]any
}

// diffField is one declared field: its values, in the declared Go type, and the
// operators it takes.
type diffField struct {
	name   string
	typ    filter.Type
	values []any
	ops    []filter.Op
	// encode turns a value into what the row holds, which only a cbor field
	// changes.
	encode func(*testing.T, any) any
}

// diffFieldList is every field, in the table's column order.
var diffFieldList = []diffField{
	{
		name:   "title",
		typ:    filter.TypeText,
		values: []any{"Rook notes", "rook", "Notes", "a b c", "", "Zz top", "notes about rooks"},
		ops:    []filter.Op{filter.OpEq, filter.OpNe, filter.OpLt, filter.OpLe, filter.OpGt, filter.OpGe, filter.OpContains, filter.OpPrefix, filter.OpIn},
	},
	{
		name:   "name",
		typ:    filter.TypeString,
		values: []any{"notes", "rook", "a", ""},
		ops:    []filter.Op{filter.OpEq, filter.OpNe, filter.OpLt, filter.OpLe, filter.OpGt, filter.OpGe, filter.OpContains, filter.OpPrefix, filter.OpIn},
	},
	{
		name:   "state",
		typ:    filter.TypeEnum,
		values: []any{"open", "closed", "doing"},
		ops:    []filter.Op{filter.OpEq, filter.OpNe, filter.OpIn},
	},
	{
		name:   "about",
		typ:    filter.TypeRef,
		values: []any{"record://example/notes/01", "record://example/notes/02", "space://01"},
		ops:    []filter.Op{filter.OpEq, filter.OpNe, filter.OpContains, filter.OpPrefix, filter.OpIn},
	},
	{
		name:   "done",
		typ:    filter.TypeBool,
		values: []any{true, false},
		ops:    []filter.Op{filter.OpEq, filter.OpNe, filter.OpIn},
	},
	{
		name:   "count",
		typ:    filter.TypeInt,
		values: []any{int64(-2), int64(0), int64(3), int64(7), int64(99)},
		ops:    []filter.Op{filter.OpEq, filter.OpNe, filter.OpLt, filter.OpLe, filter.OpGt, filter.OpGe, filter.OpIn},
	},
	{
		name:   "score",
		typ:    filter.TypeFloat,
		values: []any{-1.5, 0.0, 2.25, 1000.0, 0.125},
		ops:    []filter.Op{filter.OpEq, filter.OpNe, filter.OpLt, filter.OpLe, filter.OpGt, filter.OpGe, filter.OpIn},
	},
	{
		name: "at",
		typ:  filter.TypeTime,
		values: []any{
			time.Date(2024, 3, 1, 12, 0, 0, 0, time.UTC),
			time.Date(2024, 3, 2, 12, 0, 0, 0, time.UTC),
			time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC),
		},
		ops: []filter.Op{filter.OpEq, filter.OpNe, filter.OpLt, filter.OpLe, filter.OpGt, filter.OpGe, filter.OpIn},
	},
	{
		name:   "body",
		typ:    filter.TypeCBOR,
		values: []any{map[string]any{"a": int64(1)}, "text", int64(3), []any{true, false}},
		ops:    []filter.Op{filter.OpEq, filter.OpNe},
		encode: func(t *testing.T, value any) any {
			t.Helper()
			encoded, err := cborx.Marshal(value)
			if err != nil {
				t.Fatalf("encoding %v: %v", value, err)
			}
			return encoded
		},
	},
}

// TestDifferential runs generated filters against both sides.
func TestDifferential(t *testing.T) {
	store := storetest.New(t)
	ctx := t.Context()

	if _, err := store.Pool().Exec(ctx, diffTable); err != nil {
		t.Fatalf("creating the table: %v", err)
	}

	// A fixed seed: the test needs the same filters on every run, not
	// unpredictability.
	random := rand.New(rand.NewPCG(0x5eed, 0xf11e7e)) //nolint:gosec
	records := writeDiffRecords(t, random, store)
	if len(records) != diffRows {
		t.Fatalf("read %d rows, want %d", len(records), diffRows)
	}

	filters := generateFilters(random, 400, 3)
	matchedSomething := 0
	for i, generated := range filters {
		if err := generated.Validate(diffFields); err != nil {
			t.Fatalf("filter %d (%s) is invalid: %v", i, generated, err)
		}
		clause, args, err := generated.SQL(1)
		if err != nil {
			t.Fatalf("filter %d (%s) does not compile: %v", i, generated, err)
		}

		query := "SELECT id FROM diff_records WHERE " + clause + " ORDER BY id"
		rows, err := store.Pool().Query(ctx, query, args...)
		if err != nil {
			t.Fatalf("filter %d (%s): %v", i, generated, err)
		}
		fromSQL, err := pgx.CollectRows(rows, pgx.RowTo[int64])
		if err != nil {
			t.Fatalf("filter %d (%s): reading the rows: %v", i, generated, err)
		}

		fromMemory := make([]int64, 0, len(records))
		for _, diff := range records {
			matched, err := generated.Match(diff.record)
			if err != nil {
				t.Fatalf("filter %d (%s): matching: %v", i, generated, err)
			}
			if matched {
				fromMemory = append(fromMemory, diff.id)
			}
		}
		if len(fromMemory) > 0 {
			matchedSomething++
		}

		if diff := cmp.Diff(fromMemory, fromSQL); diff != "" {
			t.Fatalf("filter %d (%s) differs between memory and SQL (-memory +sql):\n%s\nclause: %s\nargs: %v",
				i, generated, diff, clause, args)
		}
	}

	// A generator that only ever matched nothing would pass while testing
	// nothing.
	if matchedSomething < len(filters)/4 {
		t.Fatalf("only %d of %d filters matched a row", matchedSomething, len(filters))
	}
}

// writeDiffRecords fills the table with random rows, some columns null, and
// reads them back the way the matcher sees them.
func writeDiffRecords(t *testing.T, random *rand.Rand, store *store.Store) []diffRecord {
	t.Helper()
	ctx := t.Context()

	placeholders := ""
	for i := range diffFieldList {
		placeholders += fmt.Sprintf(", $%d", i+2)
	}
	insert := "INSERT INTO diff_records (id" + diffColumns() + ") VALUES ($1" + placeholders + ")"

	for row := 0; row < diffRows; row++ {
		values := make([]any, 0, len(diffFieldList)+1)
		values = append(values, int64(row+1))
		for _, field := range diffFieldList {
			if random.IntN(4) == 0 {
				values = append(values, nil)
				continue
			}
			value := field.values[random.IntN(len(field.values))]
			if field.encode != nil {
				value = field.encode(t, value)
			}
			values = append(values, value)
		}
		if _, err := store.Pool().Exec(ctx, insert, values...); err != nil {
			t.Fatalf("inserting row %d: %v", row+1, err)
		}
	}

	rows, err := store.Pool().Query(ctx, "SELECT id"+diffColumns()+" FROM diff_records ORDER BY id")
	if err != nil {
		t.Fatalf("reading the rows: %v", err)
	}
	defer rows.Close()

	records := make([]diffRecord, 0, diffRows)
	for rows.Next() {
		var id int64
		var title, name, state, about *string
		var done *bool
		var count *int64
		var score *float64
		var at *time.Time
		var body *[]byte
		err := rows.Scan(&id, &title, &name, &state, &about, &done, &count, &score, &at, &body)
		if err != nil {
			t.Fatalf("scanning a row: %v", err)
		}

		record := map[string]any{
			"title": nilIfNil(title),
			"name":  nilIfNil(name),
			"state": nilIfNil(state),
			"about": nilIfNil(about),
			"done":  nilIfNil(done),
			"count": nilIfNil(count),
			"score": nilIfNil(score),
			"at":    nilIfNil(at),
			"body":  nilIfNil(body),
		}
		records = append(records, diffRecord{id: id, record: record})
	}
	if err := rows.Err(); err != nil {
		t.Fatalf("reading the rows: %v", err)
	}
	return records
}

// diffColumns lists the fields' columns, with the comma they follow.
func diffColumns() string {
	columns := ""
	for _, field := range diffFieldList {
		columns += ", " + field.name
	}
	return columns
}

// nilIfNil turns a scanning pointer into the value or nil.
func nilIfNil[T any](pointer *T) any {
	if pointer == nil {
		return nil
	}
	return *pointer
}

// generateFilters makes a count of random filters of at most depth combinations.
func generateFilters(random *rand.Rand, count, depth int) []*filter.Filter {
	filters := make([]*filter.Filter, count)
	for i := range filters {
		filters[i] = randomFilter(random, depth)
	}
	return filters
}

// randomFilter makes a random filter.
func randomFilter(random *rand.Rand, depth int) *filter.Filter {
	if depth <= 0 || random.IntN(100) < 45 {
		return randomComparison(random)
	}
	switch random.IntN(3) {
	case 0:
		return filter.And(randomFilters(random, depth-1)...)
	case 1:
		return filter.Or(randomFilters(random, depth-1)...)
	default:
		return filter.Not(randomFilter(random, depth-1))
	}
}

// randomFilters makes one to three random filters.
func randomFilters(random *rand.Rand, depth int) []*filter.Filter {
	children := make([]*filter.Filter, 1+random.IntN(3))
	for i := range children {
		children[i] = randomFilter(random, depth)
	}
	return children
}

// randomComparison makes a random comparison on a random field.
func randomComparison(random *rand.Rand) *filter.Filter {
	field := diffFieldList[random.IntN(len(diffFieldList))]

	// A null test now and then.
	if random.IntN(8) == 0 {
		op := filter.OpEq
		if random.IntN(2) == 0 {
			op = filter.OpNe
		}
		return filter.Compare(op, field.name, nil)
	}

	op := field.ops[random.IntN(len(field.ops))]
	if op == filter.OpIn {
		values := make([]any, 1+random.IntN(3))
		for i := range values {
			values[i] = field.values[random.IntN(len(field.values))]
		}
		return filter.In(field.name, values...)
	}
	value := field.values[random.IntN(len(field.values))]
	return filter.Compare(op, field.name, value)
}
