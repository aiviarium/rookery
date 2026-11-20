package data

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"testing"
	"time"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/filter"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// testAuth is an Authorizer the tests grant by hand.
type testAuth struct {
	mu     sync.Mutex
	grants map[string]rookeryv1.Level
}

// newTestAuth returns an authorizer that grants nothing to anybody yet.
func newTestAuth() *testAuth {
	return &testAuth{grants: make(map[string]rookeryv1.Level)}
}

// Level is the principal's granted level on the resource.
func (a *testAuth) Level(_ context.Context, principal Principal, resource string) (rookeryv1.Level, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if principal.Kind == PrincipalLocal {
		return rookeryv1.Level_LEVEL_LOCAL, nil
	}
	return a.grants[principal.String()+"\x00"+resource], nil
}

// grant gives a principal a level on a resource.
func (a *testAuth) grant(principal Principal, resource string, level rookeryv1.Level) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.grants[principal.String()+"\x00"+resource] = level
}

// testCollections are what the tests declare: a space-scoped collection with
// every field type and its history, a global one, a user-scoped one, and a
// target the space collection refers to.
func testCollections() []*Collection {
	return []*Collection{
		{
			Plugin:  "example",
			Name:    "notes",
			Scope:   ScopeSpace,
			Access:  Access{Read: rookeryv1.Level_LEVEL_VIEW, Write: rookeryv1.Level_LEVEL_USE},
			History: true,
			Fields: map[string]Field{
				"title": {Type: filter.TypeText, Required: true},
				"body":  {Type: filter.TypeText},
				"done":  {Type: filter.TypeBool, Default: false},
				"count": {Type: filter.TypeInt},
				"score": {Type: filter.TypeFloat},
				"at":    {Type: filter.TypeTime},
				"state": {Type: filter.TypeEnum, Values: []string{"open", "closed"}, Default: "open"},
				"about": {Type: filter.TypeRef, OnDelete: OnDeleteNullify},
				"tags":  {Type: filter.TypeCBOR},
				"embed": {Type: filter.TypeVector, Size: 3},
			},
			Indexes: [][]string{{"done"}},
			Entity:  &Entity{Kind: "Note", Title: "title", Text: []string{"title", "body"}},
		},
		{
			Plugin: "example",
			Name:   "kinds",
			Scope:  ScopeGlobal,
			Access: Access{Read: rookeryv1.Level_LEVEL_VIEW, Write: rookeryv1.Level_LEVEL_MANAGE},
			Fields: map[string]Field{
				"label": {Type: filter.TypeString, Required: true},
			},
		},
		{
			Plugin: "example",
			Name:   "drafts",
			Scope:  ScopeUser,
			Access: Access{Read: rookeryv1.Level_LEVEL_VIEW, Write: rookeryv1.Level_LEVEL_USE},
			Fields: map[string]Field{
				"text": {Type: filter.TypeText},
			},
		},
		{
			Plugin:  "example",
			Name:    "boards",
			Scope:   ScopeSpace,
			Access:  Access{Read: rookeryv1.Level_LEVEL_VIEW, Write: rookeryv1.Level_LEVEL_USE},
			History: true,
			Fields: map[string]Field{
				"name": {Type: filter.TypeString, Required: true},
				"note": {Type: filter.TypeRef, OnDelete: OnDeleteCascade},
				"link": {Type: filter.TypeRef, OnDelete: OnDeleteRestrict},
			},
		},
	}
}

// datastore is a datastore on a fresh database, with the test declarations and
// their tables.
type datastore struct {
	*Data
	auth *testAuth
}

// newDatastore returns a datastore on a database of the test's own.
func newDatastore(t *testing.T) *datastore {
	t.Helper()

	auth := newTestAuth()
	data := New(Config{Store: storetest.New(t), Authorizer: auth})
	if err := data.Declare(testCollections()...); err != nil {
		t.Fatalf("declaring: %v", err)
	}
	if err := data.Create(t.Context(), "example"); err != nil {
		t.Fatalf("creating the tables: %v", err)
	}
	return &datastore{Data: data, auth: auth}
}

// user, plugin and local are the principals the tests act as.
var (
	user     = Principal{Kind: PrincipalUser, ID: "01USER"}
	other    = Principal{Kind: PrincipalUser, ID: "02USER"}
	plugin   = Principal{Kind: PrincipalPlugin, ID: "example"}
	stranger = Principal{Kind: PrincipalUser, ID: "03USER"}
)

// space is the space the tests work in.
const space = "01SPACE"

// grant gives a principal a level on the test space and its collections.
func (d *datastore) grant(principal Principal, resource string, level rookeryv1.Level) {
	d.auth.grant(principal, resource, level)
}

// writer grants the test user the levels a write needs in a space, and returns
// it.
func (d *datastore) writer(t *testing.T) Principal {
	t.Helper()
	d.grant(user, "space://"+space, rookeryv1.Level_LEVEL_MANAGE)
	return user
}

// insertNote writes a note and fails the test when it cannot.
func (d *datastore) insertNote(t *testing.T, principal Principal, record map[string]any) map[string]any {
	t.Helper()
	if _, ok := record["space_id"]; !ok {
		record["space_id"] = space
	}
	written, err := d.Insert(t.Context(), principal, "example/notes", record)
	if err != nil {
		t.Fatalf("inserting %v: %v", record, err)
	}
	return written
}

// TestDeclareRefuses checks the declaration validation.
func TestDeclareRefuses(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name   string
		change func(*Collection)
	}{
		{name: "plugin name", change: func(c *Collection) { c.Plugin = "Example" }},
		{name: "collection name", change: func(c *Collection) { c.Name = "9notes" }},
		{name: "system column", change: func(c *Collection) { c.Fields["rev"] = Field{Type: filter.TypeInt} }},
		{name: "scope", change: func(c *Collection) { c.Scope = "team" }},
		{name: "no level", change: func(c *Collection) { c.Access.Read = rookeryv1.Level_LEVEL_UNSPECIFIED }},
		{name: "no type", change: func(c *Collection) { c.Fields["x"] = Field{} }},
		{name: "vector size", change: func(c *Collection) { c.Fields["v"] = Field{Type: filter.TypeVector, Size: 0} }},
		{name: "enum default", change: func(c *Collection) {
			c.Fields["e"] = Field{Type: filter.TypeEnum, Values: []string{"a"}, Default: "b"}
		}},
		{name: "required nullify", change: func(c *Collection) {
			c.Fields["r"] = Field{Type: filter.TypeRef, Required: true, OnDelete: OnDeleteNullify}
		}},
		{name: "unknown index field", change: func(c *Collection) { c.Indexes = [][]string{{"missing"}} }},
		{name: "vector index", change: func(c *Collection) { c.Indexes = [][]string{{"embed"}} }},
		{name: "entity title", change: func(c *Collection) { c.Entity.Title = "missing" }},
		{name: "entity text", change: func(c *Collection) { c.Entity.Text = nil }},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()

			collection := testCollections()[0]
			test.change(collection)
			if err := collection.Validate(); !errors.Is(err, ErrInvalid) {
				t.Fatalf("Validate = %v, want ErrInvalid", err)
			}
		})
	}
}

// TestCreateTables checks the tables, their columns and their indexes.
func TestCreateTables(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	ctx := t.Context()

	columns := map[string][]string{}
	rows, err := d.Store().Pool().Query(ctx, `
		SELECT table_name, column_name FROM information_schema.columns
		WHERE table_schema = 'p_example' ORDER BY table_name, ordinal_position`)
	if err != nil {
		t.Fatalf("reading the columns: %v", err)
	}
	for rows.Next() {
		var table, column string
		if err := rows.Scan(&table, &column); err != nil {
			t.Fatalf("reading the columns: %v", err)
		}
		columns[table] = append(columns[table], column)
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		t.Fatalf("reading the columns: %v", err)
	}

	want := []string{
		"id", "rev", "created_at", "updated_at", "deleted_at", "space_id",
		"about", "at", "body", "count", "done", "embed", "score", "state", "tags", "title",
	}
	if diff := fmt.Sprint(columns["notes"]); diff != fmt.Sprint(want) {
		t.Errorf("notes has the columns %v, want %v", columns["notes"], want)
	}
	if _, ok := columns["notes_history"]; !ok {
		t.Error("notes_history was not created")
	}
	if _, ok := columns["kinds"]; !ok {
		t.Error("kinds was not created")
	}

	var historyTable string
	err = d.Store().Pool().QueryRow(ctx, `
		SELECT to_regclass('p_example.notes_history')::text`).Scan(&historyTable)
	if err != nil {
		t.Fatalf("looking for the history table: %v", err)
	}
	var index string
	err = d.Store().Pool().QueryRow(ctx, `
		SELECT indexname FROM pg_indexes WHERE schemaname = 'p_example' AND indexname = 'notes_done_idx'`).Scan(&index)
	if err != nil {
		t.Fatalf("looking for the index: %v", err)
	}
}

// TestInsertAndGet checks a record's journey through a write and a read.
func TestInsertAndGet(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	ctx := t.Context()
	principal := d.writer(t)

	written := d.insertNote(t, principal, map[string]any{
		"title": "Rook notes",
		"body":  "the first",
		"count": int64(2),
		"tags":  map[string]any{"a": int64(1)},
		"embed": []any{1.0, 2.0, 3.0},
	})
	if written["id"] == "" {
		t.Fatal("the record has no id")
	}
	if written["rev"] != int64(1) {
		t.Errorf("rev = %v, want 1", written["rev"])
	}
	if written["done"] != false || written["state"] != "open" {
		t.Errorf("defaults are %v and %v, want false and open", written["done"], written["state"])
	}
	if written["space_id"] != space {
		t.Errorf("space_id = %v, want %s", written["space_id"], space)
	}

	read, err := d.Get(ctx, principal, "example/notes", written["id"].(string))
	if err != nil {
		t.Fatalf("getting: %v", err)
	}
	for _, field := range []string{"title", "body", "count", "state", "tags", "embed", "created_at", "deleted_at"} {
		if fmt.Sprint(read[field]) != fmt.Sprint(written[field]) {
			t.Errorf("%s: read %v, wrote %v", field, read[field], written[field])
		}
	}

	// The entity row is there, with the title and the searchable text.
	var kind, title, body, access string
	err = d.Store().Pool().QueryRow(ctx, `SELECT kind, title, body, access FROM rookery.entities WHERE uri = $1`,
		"record://example/notes/"+written["id"].(string)).Scan(&kind, &title, &body, &access)
	if err != nil {
		t.Fatalf("reading the entity: %v", err)
	}
	if kind != "Note" || title != "Rook notes" || body != "Rook notes the first" || access != "space://"+space {
		t.Errorf("entity = %q %q %q %q", kind, title, body, access)
	}

	// The change row is there, with the record as CBOR.
	var changeKind string
	var rev int64
	var data []byte
	err = d.Store().Pool().QueryRow(ctx,
		`SELECT kind, rev, data FROM rookery.changes WHERE resource = $1`, "record://example/notes/"+written["id"].(string)).
		Scan(&changeKind, &rev, &data)
	if err != nil {
		t.Fatalf("reading the change: %v", err)
	}
	if changeKind != "insert" || rev != 1 || len(data) == 0 {
		t.Errorf("change = %q %d %d bytes", changeKind, rev, len(data))
	}

	// The history row is there too.
	var historyRev int64
	err = d.Store().Pool().QueryRow(ctx,
		`SELECT rev FROM p_example.notes_history WHERE id = $1`, written["id"]).Scan(&historyRev)
	if err != nil {
		t.Fatalf("reading the history: %v", err)
	}
	if historyRev != 1 {
		t.Errorf("history rev = %d, want 1", historyRev)
	}
}

// TestInsertRefuses checks the validation a write does.
func TestInsertRefuses(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	ctx := t.Context()
	principal := d.writer(t)

	tests := []struct {
		name   string
		record map[string]any
		errors error
	}{
		{name: "required", record: map[string]any{"body": "x"}, errors: ErrInvalid},
		{name: "unknown field", record: map[string]any{"title": "x", "missing": 1}, errors: ErrInvalid},
		{name: "wrong type", record: map[string]any{"title": "x", "count": "two"}, errors: ErrInvalid},
		{name: "enum", record: map[string]any{"title": "x", "state": "gone"}, errors: ErrInvalid},
		{name: "ref", record: map[string]any{"title": "x", "about": "not a uri"}, errors: ErrInvalid},
		{name: "vector size", record: map[string]any{"title": "x", "embed": []any{1.0, 2.0}}, errors: ErrInvalid},
		{name: "space", record: map[string]any{"title": "x"}, errors: nil},
		{name: "no space", record: map[string]any{"title": "x", "space_id": ""}, errors: ErrInvalid},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			// The tests share the datastore, so each has its own transaction
			// through one insert.
			if test.name == "space" {
				// A space-scoped record without one is refused because the
				// helper fills it in; drop that case's helper.
				_, err := d.Insert(ctx, principal, "example/notes", map[string]any{"title": "x"})
				if !errors.Is(err, ErrInvalid) {
					t.Fatalf("Insert without a space = %v, want ErrInvalid", err)
				}
				return
			}
			record := test.record
			if _, ok := record["space_id"]; !ok {
				record["space_id"] = space
			}
			if _, err := d.Insert(ctx, principal, "example/notes", record); !errors.Is(err, test.errors) {
				t.Fatalf("Insert = %v, want %v", err, test.errors)
			}
		})
	}
}

// TestUpdateRevision checks the revision check and that a patch leaves the rest.
func TestUpdateRevision(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	ctx := t.Context()
	principal := d.writer(t)

	written := d.insertNote(t, principal, map[string]any{"title": "one", "count": int64(1)})
	id := written["id"].(string)

	updated, err := d.Update(ctx, principal, "example/notes", id, map[string]any{"body": "two"}, 1)
	if err != nil {
		t.Fatalf("updating: %v", err)
	}
	if updated["rev"] != int64(2) || updated["title"] != "one" || updated["body"] != "two" {
		t.Errorf("updated = %v", updated)
	}

	if _, err := d.Update(ctx, principal, "example/notes", id, map[string]any{"body": "three"}, 1); !errors.Is(err, ErrRevision) {
		t.Fatalf("updating at the old revision = %v, want ErrRevision", err)
	}
	var revision *RevisionError
	if _, err := d.Update(ctx, principal, "example/notes", id, map[string]any{"body": "three"}, 7); !errors.As(err, &revision) ||
		revision.Expected != 7 || revision.Actual != 2 {
		t.Fatalf("the revision error is %v", err)
	}

	if _, err := d.Update(ctx, principal, "example/notes", id, map[string]any{"space_id": "other"}, 2); !errors.Is(err, ErrInvalid) {
		t.Fatalf("moving a record = %v, want ErrInvalid", err)
	}

	// Every write left a change, in order.
	rows, err := d.Store().Pool().Query(ctx, `SELECT kind, rev FROM rookery.changes WHERE resource = $1 ORDER BY seq`,
		"record://example/notes/"+id)
	if err != nil {
		t.Fatalf("reading the changes: %v", err)
	}
	defer rows.Close()
	var kinds []string
	for rows.Next() {
		var kind string
		var rev int64
		if err := rows.Scan(&kind, &rev); err != nil {
			t.Fatalf("reading the changes: %v", err)
		}
		kinds = append(kinds, fmt.Sprintf("%s:%d", kind, rev))
	}
	if diff := fmt.Sprint(kinds); diff != fmt.Sprint([]string{"insert:1", "update:2"}) {
		t.Errorf("changes = %v", kinds)
	}
}

// TestDeleteRestore checks the soft delete, the history and the entity row.
func TestDeleteRestore(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	ctx := t.Context()
	principal := d.writer(t)

	written := d.insertNote(t, principal, map[string]any{"title": "one"})
	id := written["id"].(string)
	uri := "record://example/notes/" + id

	if err := d.Delete(ctx, principal, "example/notes", id, 1); err != nil {
		t.Fatalf("deleting: %v", err)
	}
	if _, err := d.Get(ctx, principal, "example/notes", id); !errors.Is(err, ErrNotFound) {
		t.Fatalf("getting a deleted record = %v, want ErrNotFound", err)
	}
	if _, err := d.Update(ctx, principal, "example/notes", id, map[string]any{"title": "x"}, 0); !errors.Is(err, ErrNotFound) {
		t.Fatalf("updating a deleted record = %v, want ErrNotFound", err)
	}

	var deleted bool
	var rev int64
	if err := d.Store().Pool().QueryRow(ctx,
		`SELECT deleted, rev FROM p_example.notes_history WHERE id = $1 ORDER BY rev DESC LIMIT 1`, id).
		Scan(&deleted, &rev); err != nil {
		t.Fatalf("reading the history: %v", err)
	}
	if !deleted || rev != 2 {
		t.Errorf("the history's last row is deleted=%v rev=%d, want true 2", deleted, rev)
	}

	var entityURI string
	err := d.Store().Pool().QueryRow(ctx, `SELECT uri FROM rookery.entities WHERE uri = $1`, uri).Scan(&entityURI)
	if err == nil {
		t.Fatal("the deleted record's entity is still there")
	}

	restored, err := d.Restore(ctx, principal, "example/notes", id, 2)
	if err != nil {
		t.Fatalf("restoring: %v", err)
	}
	if restored["rev"] != int64(3) || restored["deleted_at"] != nil {
		t.Errorf("restored = %v", restored)
	}
	if _, err := d.Get(ctx, principal, "example/notes", id); err != nil {
		t.Fatalf("getting a restored record: %v", err)
	}
	if _, err := d.Restore(ctx, principal, "example/notes", id, 3); !errors.Is(err, ErrInvalid) {
		t.Fatalf("restoring a live record = %v, want ErrInvalid", err)
	}

	// The history lists every revision, oldest first.
	history, err := d.History(ctx, principal, "example/notes", id, 0, 10)
	if err != nil {
		t.Fatalf("history: %v", err)
	}
	if len(history) != 3 {
		t.Fatalf("history has %d entries, want 3", len(history))
	}
	if history[0]["rev"] != int64(1) || history[1]["deleted"] != true || history[2]["rev"] != int64(3) {
		t.Errorf("history = %v", history)
	}
}

// TestFindPages checks the filter, the order, the limit and the cursor.
func TestFindPages(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	ctx := t.Context()
	principal := d.writer(t)

	for i, title := range []string{"d", "b", "a", "c"} {
		d.insertNote(t, principal, map[string]any{
			"title": title,
			"count": int64(i),
			"done":  i%2 == 0,
		})
	}
	// A note in another space, which the space query must not see.
	d.grant(user, "space://02SPACE", rookeryv1.Level_LEVEL_MANAGE)
	if _, err := d.Insert(ctx, principal, "example/notes", map[string]any{
		"title": "elsewhere", "space_id": "02SPACE",
	}); err != nil {
		t.Fatalf("inserting elsewhere: %v", err)
	}

	all, err := d.Find(ctx, principal, "example/notes", Query{Space: space, Sorts: []Sort{{Field: "title"}}})
	if err != nil {
		t.Fatalf("finding: %v", err)
	}
	if len(all.Records) != 4 || all.Records[0]["title"] != "a" || all.Cursor != "" {
		t.Fatalf("find = %v, cursor %q", titles(all.Records), all.Cursor)
	}

	// A page of two, then the next page of two.
	first, err := d.Find(ctx, principal, "example/notes", Query{Space: space, Sorts: []Sort{{Field: "title"}}, Limit: 2})
	if err != nil {
		t.Fatalf("finding: %v", err)
	}
	if len(first.Records) != 2 || first.Records[0]["title"] != "a" || first.Cursor == "" {
		t.Fatalf("first page = %v, cursor %q", titles(first.Records), first.Cursor)
	}
	second, err := d.Find(ctx, principal, "example/notes", Query{
		Space: space, Sorts: []Sort{{Field: "title"}}, Limit: 2, Cursor: first.Cursor,
	})
	if err != nil {
		t.Fatalf("finding the second page: %v", err)
	}
	if len(second.Records) != 2 || second.Records[0]["title"] != "c" || second.Cursor != "" {
		t.Fatalf("second page = %v, cursor %q", titles(second.Records), second.Cursor)
	}

	// The same in descending order.
	desc, err := d.Find(ctx, principal, "example/notes", Query{
		Space: space, Sorts: []Sort{{Field: "title", Desc: true}}, Limit: 3,
	})
	if err != nil {
		t.Fatalf("finding: %v", err)
	}
	if diff := fmt.Sprint(titles(desc.Records)); diff != fmt.Sprint([]string{"d", "c", "b"}) {
		t.Fatalf("descending = %v", titles(desc.Records))
	}
	next, err := d.Find(ctx, principal, "example/notes", Query{
		Space: space, Sorts: []Sort{{Field: "title", Desc: true}}, Limit: 3, Cursor: desc.Cursor,
	})
	if err != nil {
		t.Fatalf("finding: %v", err)
	}
	if diff := fmt.Sprint(titles(next.Records)); diff != fmt.Sprint([]string{"a"}) {
		t.Fatalf("the descending next page = %v", titles(next.Records))
	}

	// A filter, and the count.
	filtered, err := filter.ParseText("done = true and count >= 0", d.mustCollection(t, "example/notes").FilterFields())
	if err != nil {
		t.Fatalf("parsing the filter: %v", err)
	}
	found, err := d.Find(ctx, principal, "example/notes", Query{Space: space, Filter: filtered})
	if err != nil {
		t.Fatalf("finding: %v", err)
	}
	if len(found.Records) != 2 {
		t.Errorf("the filtered find = %v", titles(found.Records))
	}
	count, err := d.Count(ctx, principal, "example/notes", Query{Space: space, Filter: filtered})
	if err != nil {
		t.Fatalf("counting: %v", err)
	}
	if count != 2 {
		t.Errorf("count = %d, want 2", count)
	}
}

// TestAggregate checks count, sum, min, max and avg, grouped and not.
func TestAggregate(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	ctx := t.Context()
	principal := d.writer(t)

	for i, state := range []string{"open", "open", "closed"} {
		d.insertNote(t, principal, map[string]any{
			"title": "note", "state": state, "count": int64(i + 1), "score": float64(i) + 0.5,
		})
	}

	tests := []struct {
		name      string
		aggregate Aggregate
		want      any
		groups    int
	}{
		{name: "count", aggregate: Aggregate{Op: AggregateCount}, want: int64(3), groups: 1},
		{name: "sum", aggregate: Aggregate{Op: AggregateSum, Field: "count"}, want: 6.0, groups: 1},
		{name: "avg", aggregate: Aggregate{Op: AggregateAvg, Field: "count"}, want: 2.0, groups: 1},
		{name: "min", aggregate: Aggregate{Op: AggregateMin, Field: "score"}, want: 0.5, groups: 1},
		{name: "max", aggregate: Aggregate{Op: AggregateMax, Field: "count"}, want: int64(3), groups: 1},
		{
			name:      "grouped",
			aggregate: Aggregate{Op: AggregateCount, GroupBy: []string{"state"}},
			want:      int64(1),
			groups:    2,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			test.aggregate.Space = space
			results, err := d.Aggregate(ctx, principal, "example/notes", test.aggregate)
			if err != nil {
				t.Fatalf("aggregating: %v", err)
			}
			if len(results) != test.groups {
				t.Fatalf("%d groups, want %d: %v", len(results), test.groups, results)
			}
			if fmt.Sprint(results[0].Value) != fmt.Sprint(test.want) {
				t.Errorf("value = %v, want %v", results[0].Value, test.want)
			}
		})
	}

	// A group carries its field's value.
	grouped, err := d.Aggregate(ctx, principal, "example/notes", Aggregate{
		Op: AggregateCount, GroupBy: []string{"state"}, Space: space,
	})
	if err != nil {
		t.Fatalf("aggregating: %v", err)
	}
	if grouped[0].Group["state"] != "closed" || grouped[0].Value != int64(1) {
		t.Errorf("the closed group = %v", grouped[0])
	}
	if grouped[1].Group["state"] != "open" || grouped[1].Value != int64(2) {
		t.Errorf("the open group = %v", grouped[1])
	}
}

// TestReferences checks the reference rows and the rules for deleting a
// referenced record.
func TestReferences(t *testing.T) {
	t.Parallel()

	t.Run("nullify", func(t *testing.T) {
		t.Parallel()

		d := newDatastore(t)
		ctx := t.Context()
		principal := d.writer(t)

		target := d.insertNote(t, principal, map[string]any{"title": "target"})
		holder := d.insertNote(t, principal, map[string]any{
			"title": "holder", "about": "record://example/notes/" + target["id"].(string),
		})
		if err := d.Delete(ctx, principal, "example/notes", target["id"].(string), 0); err != nil {
			t.Fatalf("deleting the target: %v", err)
		}
		cleared, err := d.Get(ctx, principal, "example/notes", holder["id"].(string))
		if err != nil {
			t.Fatalf("getting the holder: %v", err)
		}
		if cleared["about"] != nil {
			t.Errorf("the holder's reference is %v, want null", cleared["about"])
		}
	})

	t.Run("restrict and cascade", func(t *testing.T) {
		t.Parallel()

		d := newDatastore(t)
		ctx := t.Context()
		principal := d.writer(t)

		target := d.insertNote(t, principal, map[string]any{"title": "target"})
		targetURI := "record://example/notes/" + target["id"].(string)

		board, err := d.Insert(ctx, principal, "example/boards", map[string]any{
			"name": "board", "space_id": space, "note": targetURI,
		})
		if err != nil {
			t.Fatalf("inserting a board: %v", err)
		}

		refs, err := d.RefsTo(ctx, principal, targetURI)
		if err != nil {
			t.Fatalf("refs to: %v", err)
		}
		if len(refs) != 1 || refs[0].Field != "note" || refs[0].OnDelete != OnDeleteCascade {
			t.Fatalf("refs to = %v", refs)
		}
		from, err := d.RefsFrom(ctx, principal, "record://example/boards/"+board["id"].(string))
		if err != nil {
			t.Fatalf("refs from: %v", err)
		}
		if len(from) != 1 || from[0].Target != targetURI {
			t.Fatalf("refs from = %v", from)
		}

		// The cascade deletes the board with the note.
		if err := d.Delete(ctx, principal, "example/notes", target["id"].(string), 0); err != nil {
			t.Fatalf("deleting the target: %v", err)
		}
		if _, err := d.Get(ctx, principal, "example/boards", board["id"].(string)); !errors.Is(err, ErrNotFound) {
			t.Fatalf("the board survived its cascade: %v", err)
		}
	})

	t.Run("restrict", func(t *testing.T) {
		t.Parallel()

		d := newDatastore(t)
		ctx := t.Context()
		principal := d.writer(t)

		link := "space://" + space
		board, err := d.Insert(ctx, principal, "example/boards", map[string]any{
			"name": "board", "space_id": space, "link": link,
		})
		if err != nil {
			t.Fatalf("inserting a board: %v", err)
		}

		// A restrict reference to a record refuses the delete.
		target := d.insertNote(t, principal, map[string]any{"title": "target"})
		targetURI := "record://example/notes/" + target["id"].(string)
		if _, err := d.Update(ctx, principal, "example/boards", board["id"].(string),
			map[string]any{"note": targetURI}, 0); err != nil {
			t.Fatalf("updating the board: %v", err)
		}
		// note is a cascade; make it restrict by using link, which is.
		if _, err := d.Update(ctx, principal, "example/boards", board["id"].(string),
			map[string]any{"note": nil}, 0); err != nil {
			t.Fatalf("clearing the board's note: %v", err)
		}

		// Deleting a record a restrict reference holds is refused.
		ref := d.insertNote(t, principal, map[string]any{"title": "linked"})
		refURI := "record://example/notes/" + ref["id"].(string)
		if _, err := d.Update(ctx, principal, "example/boards", board["id"].(string),
			map[string]any{"link": refURI}, 0); err != nil {
			t.Fatalf("updating the board: %v", err)
		}
		err = d.Delete(ctx, principal, "example/notes", ref["id"].(string), 0)
		if !errors.Is(err, ErrReference) {
			t.Fatalf("deleting a restricted record = %v, want ErrReference", err)
		}
		var reference *ReferenceError
		if !errors.As(err, &reference) || reference.Field != "link" {
			t.Fatalf("the reference error is %v", err)
		}
	})
}

// TestAccess checks that the scope's level decides every operation.
func TestAccess(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	ctx := t.Context()

	// Nobody has a level yet.
	if _, err := d.Insert(ctx, user, "example/notes", map[string]any{"title": "x", "space_id": space}); !errors.Is(err, ErrForbidden) {
		t.Fatalf("inserting without a level = %v, want ErrForbidden", err)
	}

	// Read only.
	d.grant(user, "space://"+space, rookeryv1.Level_LEVEL_VIEW)
	if _, err := d.Insert(ctx, user, "example/notes", map[string]any{"title": "x", "space_id": space}); !errors.Is(err, ErrForbidden) {
		t.Fatalf("inserting with view = %v, want ErrForbidden", err)
	}

	// A record in another space is not seen, and a query needs a space.
	d.grant(other, "space://02SPACE", rookeryv1.Level_LEVEL_MANAGE)
	written, err := d.Insert(ctx, other, "example/notes", map[string]any{"title": "elsewhere", "space_id": "02SPACE"})
	if err != nil {
		t.Fatalf("inserting elsewhere: %v", err)
	}
	if _, err := d.Get(ctx, user, "example/notes", written["id"].(string)); !errors.Is(err, ErrForbidden) {
		t.Fatalf("getting across spaces = %v, want ErrForbidden", err)
	}
	if _, err := d.Find(ctx, user, "example/notes", Query{}); !errors.Is(err, ErrInvalid) {
		t.Fatalf("finding without a space = %v, want ErrInvalid", err)
	}

	// A global collection is checked on the plugin's resource, once.
	if _, err := d.Insert(ctx, plugin, "example/kinds", map[string]any{"label": "x"}); !errors.Is(err, ErrForbidden) {
		t.Fatalf("a plugin inserting into its own collection = %v, want ErrForbidden", err)
	}
	d.grant(plugin, "plugin://example", rookeryv1.Level_LEVEL_MANAGE)
	if _, err := d.Insert(ctx, plugin, "example/kinds", map[string]any{"label": "x"}); err != nil {
		t.Fatalf("a granted plugin inserting: %v", err)
	}
	if _, err := d.Find(ctx, user, "example/kinds", Query{}); !errors.Is(err, ErrForbidden) {
		t.Fatalf("reading a global collection without a level = %v, want ErrForbidden", err)
	}
	d.grant(user, "plugin://example", rookeryv1.Level_LEVEL_VIEW)
	if _, err := d.Find(ctx, user, "example/kinds", Query{}); err != nil {
		t.Fatalf("reading a global collection with view: %v", err)
	}
}

// TestUserScope checks that a user reads and writes its own records, and that a
// plugin acting alone does not read a user's.
func TestUserScope(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	ctx := t.Context()

	d.grant(user, "user://"+user.ID, rookeryv1.Level_LEVEL_SELF)
	d.grant(other, "user://"+other.ID, rookeryv1.Level_LEVEL_SELF)

	written, err := d.Insert(ctx, user, "example/drafts", map[string]any{"text": "mine"})
	if err != nil {
		t.Fatalf("inserting a draft: %v", err)
	}
	if written["owner_id"] != user.ID {
		t.Errorf("owner_id = %v, want %s", written["owner_id"], user.ID)
	}
	if _, err := d.Insert(ctx, other, "example/drafts", map[string]any{"text": "theirs"}); err != nil {
		t.Fatalf("inserting the other draft: %v", err)
	}

	page, err := d.Find(ctx, user, "example/drafts", Query{})
	if err != nil {
		t.Fatalf("finding drafts: %v", err)
	}
	if len(page.Records) != 1 || page.Records[0]["text"] != "mine" {
		t.Errorf("the user's drafts = %v", page.Records)
	}
	if _, err := d.Get(ctx, user, "example/drafts", written["id"].(string)); err != nil {
		t.Fatalf("getting the own draft: %v", err)
	}
	if _, err := d.Find(ctx, plugin, "example/drafts", Query{}); !errors.Is(err, ErrForbidden) {
		t.Fatalf("a plugin reading drafts = %v, want ErrForbidden", err)
	}
	d.grant(plugin, "user://"+user.ID, rookeryv1.Level_LEVEL_USE)
	if _, err := d.Insert(ctx, plugin, "example/drafts", map[string]any{"text": "x", "owner_id": user.ID}); err != nil {
		t.Fatalf("a plugin writing a user's draft: %v", err)
	}
}

// TestBatch checks that a batch is one transaction.
func TestBatch(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	ctx := t.Context()
	principal := d.writer(t)

	written, err := d.Batch(ctx, principal, "example/notes", []BatchOp{
		{Kind: BatchInsert, Record: map[string]any{"title": "one", "space_id": space}},
		{Kind: BatchInsert, Record: map[string]any{"title": "two", "space_id": space}},
	})
	if err != nil {
		t.Fatalf("batching: %v", err)
	}
	if len(written) != 2 {
		t.Fatalf("the batch wrote %d records, want 2", len(written))
	}

	_, err = d.Batch(ctx, principal, "example/notes", []BatchOp{
		{Kind: BatchInsert, Record: map[string]any{"title": "three", "space_id": space}},
		{Kind: BatchInsert, Record: map[string]any{"space_id": space}},
	})
	if !errors.Is(err, ErrInvalid) {
		t.Fatalf("a batch with a bad operation = %v, want ErrInvalid", err)
	}
	count, err := d.Count(ctx, principal, "example/notes", Query{Space: space})
	if err != nil {
		t.Fatalf("counting: %v", err)
	}
	if count != 2 {
		t.Errorf("the failed batch left %d records, want 2", count)
	}
}

// TestUpsert checks insert or update.
func TestUpsert(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	ctx := t.Context()
	principal := d.writer(t)

	first, err := d.Upsert(ctx, principal, "example/notes", map[string]any{
		"id": "01NOTE", "title": "one", "space_id": space,
	}, 0)
	if err != nil {
		t.Fatalf("upserting a new record: %v", err)
	}
	if first["rev"] != int64(1) {
		t.Errorf("rev = %v, want 1", first["rev"])
	}

	second, err := d.Upsert(ctx, principal, "example/notes", map[string]any{
		"id": "01NOTE", "title": "two", "space_id": space,
	}, 1)
	if err != nil {
		t.Fatalf("upserting an existing record: %v", err)
	}
	if second["rev"] != int64(2) || second["title"] != "two" {
		t.Errorf("the upserted record = %v", second)
	}

	if _, err := d.Upsert(ctx, principal, "example/notes", map[string]any{
		"title": "three", "space_id": space,
	}, 0); err != nil {
		t.Fatalf("upserting without an id: %v", err)
	}
}

// mustCollection returns a declared collection or fails.
func (d *Data) mustCollection(t *testing.T, key string) *Collection {
	t.Helper()
	collection, err := d.Collection(key)
	if err != nil {
		t.Fatalf("collection %s: %v", key, err)
	}
	return collection
}

// titles is the records' titles, for error messages.
func titles(records []map[string]any) []string {
	out := make([]string, len(records))
	for i, record := range records {
		out[i], _ = record["title"].(string)
	}
	return out
}

// TestNewID checks that ids sort by creation time.
func TestNewID(t *testing.T) {
	t.Parallel()

	first, err := newID()
	if err != nil {
		t.Fatalf("newID: %v", err)
	}
	time.Sleep(time.Millisecond)
	second, err := newID()
	if err != nil {
		t.Fatalf("newID: %v", err)
	}
	if len(first) != 26 || first >= second {
		t.Errorf("ids %q and %q", first, second)
	}
}
