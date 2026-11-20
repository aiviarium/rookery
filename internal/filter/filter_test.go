package filter

import (
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"

	"github.com/aiviarium/rookery/internal/cborx"
)

// testFields are the fields the tests filter on: one per filterable type.
var testFields = Fields{
	"title":  TypeText,
	"name":   TypeString,
	"state":  TypeEnum,
	"about":  TypeRef,
	"done":   TypeBool,
	"count":  TypeInt,
	"score":  TypeFloat,
	"at":     TypeTime,
	"body":   TypeCBOR,
	"vector": TypeVector,
}

// testTime is a fixed moment for time comparisons.
var testTime = time.Date(2024, 3, 1, 12, 0, 0, 0, time.UTC)

// TestParseText checks the text form parses to the structure it names.
func TestParseText(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name  string
		text  string
		want  string
		field string
	}{
		{name: "equal", text: "done = true", want: "done = true"},
		{name: "default op is equality", text: "state open", want: "state = open"},
		{name: "not equal", text: "count != 3", want: "count != 3"},
		{name: "angle not equal", text: "count <> 3", want: "count != 3"},
		{name: "less", text: "count < 3", want: "count lt 3"},
		{name: "less or equal", text: "count <= 3", want: "count le 3"},
		{name: "greater", text: "score > 1.5", want: "score gt 1.5"},
		{name: "greater or equal", text: "score >= -2e3", want: "score ge -2000"},
		{name: "contains", text: `title contains "rook"`, want: "title contains rook"},
		{name: "prefix", text: "title prefix roo", want: "title prefix roo"},
		{name: "in", text: "state in [open, closed]", want: "state in [open, closed]"},
		{name: "in numbers", text: "count in [1, 2, 3]", want: "count in [1, 2, 3]"},
		{name: "is null", text: "about is null", want: "about is null"},
		{name: "is not null", text: "about is not null", want: "about is not null"},
		{name: "and", text: "done = true and count > 2", want: "done = true and count gt 2"},
		{name: "or", text: "done or count > 2", want: "done = true or count gt 2"},
		{name: "not", text: "not done", want: "not done = true"},
		{name: "parens", text: "not (done and count > 2)", want: "not (done = true and count gt 2)"},
		{name: "precedence", text: "done or count > 2 and score < 1", want: "done = true or (count gt 2 and score lt 1)"},
		{name: "keyword op", text: "title eq rook", want: "title = rook"},
		{name: "quoted with space", text: `name = "a b"`, want: `name = "a b"`},
		{name: "escaped", text: `name = "a\"b"`, want: `name = "a\"b"`},
		{name: "ref", text: "about = record://example/notes/01", want: "about = record://example/notes/01"},
		{name: "time", text: `at > "2024-03-01T12:00:00Z"`, want: `at gt "2024-03-01T12:00:00Z"`},
		{name: "empty", text: "", want: ""},
		{name: "spaces", text: "   ", want: ""},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()

			parsed, err := ParseText(test.text, testFields)
			if err != nil {
				t.Fatalf("ParseText(%q): %v", test.text, err)
			}
			if got := parsed.String(); got != test.want {
				t.Errorf("ParseText(%q) = %q, want %q", test.text, got, test.want)
			}
		})
	}
}

// TestParseTextErrors checks the text form refuses what it should.
func TestParseTextErrors(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name   string
		text   string
		errors error
	}{
		{name: "unknown field", text: "missing = 1", errors: ErrUnknownField},
		{name: "vector", text: "vector = 1", errors: ErrUnsupported},
		{name: "contains on int", text: "count contains 1", errors: ErrUnsupported},
		{name: "order on bool", text: "done < true", errors: ErrUnsupported},
		{name: "wrong value type", text: `count = "one"`, errors: ErrUnsupported},
		{name: "in on single value", text: "count in 1", errors: ErrInvalid},
		{name: "bare non-bool field", text: "count", errors: ErrUnsupported},
		{name: "dangling and", text: "done and", errors: ErrInvalid},
		{name: "unclosed paren", text: "(done", errors: ErrInvalid},
		{name: "unclosed string", text: `title = "x`, errors: ErrInvalid},
		{name: "trailing", text: "done true count", errors: ErrInvalid},
		{name: "order against null", text: "count is null and count > 1", errors: nil},
		{name: "is what", text: "count is maybe", errors: ErrInvalid},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()

			_, err := ParseText(test.text, testFields)
			switch {
			case test.errors == nil && err != nil:
				t.Fatalf("ParseText(%q): unexpected error %v", test.text, err)
			case test.errors != nil && !errors.Is(err, test.errors):
				t.Fatalf("ParseText(%q) = %v, want %v", test.text, err, test.errors)
			}
		})
	}
}

// TestParseStructure checks the CBOR form parses to the same structure as the
// text form.
func TestParseStructure(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name  string
		value any
		want  string
	}{
		{
			name:  "comparison",
			value: map[string]any{"field": "done", "op": "eq", "value": true},
			want:  "done = true",
		},
		{
			name:  "default op",
			value: map[string]any{"field": "count", "value": int64(3)},
			want:  "count = 3",
		},
		{
			name:  "in",
			value: map[string]any{"field": "state", "op": "in", "value": []any{"open", "closed"}},
			want:  "state in [open, closed]",
		},
		{
			name:  "null",
			value: map[string]any{"field": "about", "op": "eq", "value": nil},
			want:  "about is null",
		},
		{
			name:  "not",
			value: map[string]any{"not": map[string]any{"field": "done", "value": false}},
			want:  "not done = false",
		},
		{
			name: "and of two",
			value: map[string]any{"and": []any{
				map[string]any{"field": "done", "value": true},
				map[string]any{"field": "count", "op": "gt", "value": int64(2)},
			}},
			want: "done = true and count gt 2",
		},
		{
			name: "top-level array",
			value: []any{
				map[string]any{"field": "done", "value": true},
				map[string]any{"field": "count", "op": "gt", "value": int64(2)},
			},
			want: "done = true and count gt 2",
		},
		{
			name:  "time tag",
			value: map[string]any{"field": "at", "op": "gt", "value": testTime},
			want:  `at gt "2024-03-01T12:00:00Z"`,
		},
		{
			name:  "time text",
			value: map[string]any{"field": "at", "op": "gt", "value": "2024-03-01T12:00:00Z"},
			want:  `at gt "2024-03-01T12:00:00Z"`,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()

			encoded, err := cborx.Marshal(test.value)
			if err != nil {
				t.Fatalf("encoding: %v", err)
			}
			parsed, err := Parse(encoded, testFields)
			if err != nil {
				t.Fatalf("Parse: %v", err)
			}
			if got := parsed.String(); got != test.want {
				t.Errorf("Parse = %q, want %q", got, test.want)
			}

			// The structure Marshal writes parses back to the same filter.
			again, err := Marshal(parsed)
			if err != nil {
				t.Fatalf("Marshal: %v", err)
			}
			round, err := Parse(again, testFields)
			if err != nil {
				t.Fatalf("Parse of Marshal: %v", err)
			}
			if diff := cmp.Diff(parsed.String(), round.String()); diff != "" {
				t.Errorf("round trip differs (-want +got):\n%s", diff)
			}
		})
	}
}

// TestParseStructureErrors checks the CBOR form refuses what it should.
func TestParseStructureErrors(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name   string
		value  any
		errors error
	}{
		{name: "empty map", value: map[string]any{}, errors: ErrInvalid},
		{name: "two combinations", value: map[string]any{"and": []any{}, "or": []any{}}, errors: ErrInvalid},
		{name: "combination with field", value: map[string]any{"and": []any{}, "field": "done"}, errors: ErrInvalid},
		{name: "no value", value: map[string]any{"field": "done"}, errors: ErrInvalid},
		{name: "unknown key", value: map[string]any{"field": "done", "value": true, "extra": 1}, errors: ErrInvalid},
		{name: "unknown op", value: map[string]any{"field": "done", "op": "like", "value": true}, errors: ErrInvalid},
		{name: "in not a list", value: map[string]any{"field": "count", "op": "in", "value": 1}, errors: ErrInvalid},
		{name: "empty and", value: map[string]any{"and": []any{}}, errors: ErrInvalid},
		{name: "array of one bad", value: []any{map[string]any{"field": "missing", "value": 1}}, errors: ErrUnknownField},
		{name: "nested not", value: map[string]any{"not": map[string]any{"not": map[string]any{"field": "vector", "value": 1}}}, errors: ErrUnsupported},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()

			encoded, err := cborx.Marshal(test.value)
			if err != nil {
				t.Fatalf("encoding: %v", err)
			}
			if _, err := Parse(encoded, testFields); !errors.Is(err, test.errors) {
				t.Fatalf("Parse = %v, want %v", err, test.errors)
			}
		})
	}
}

// TestUnvalidated checks that a filter has to be validated before it is used.
func TestUnvalidated(t *testing.T) {
	t.Parallel()

	unchecked := Compare(OpEq, "done", true)
	if _, _, err := unchecked.SQL(1); !errors.Is(err, ErrUnvalidated) {
		t.Errorf("SQL of an unvalidated filter = %v, want ErrUnvalidated", err)
	}
	if _, err := unchecked.Match(nil); !errors.Is(err, ErrUnvalidated) {
		t.Errorf("Match of an unvalidated filter = %v, want ErrUnvalidated", err)
	}
	if err := unchecked.Validate(testFields); err != nil {
		t.Fatalf("Validate: %v", err)
	}
	if _, _, err := unchecked.SQL(1); err != nil {
		t.Errorf("SQL after Validate: %v", err)
	}
}

// TestNilMatchesEverything checks that no filter is no condition.
func TestNilMatchesEverything(t *testing.T) {
	t.Parallel()

	var none *Filter
	matched, err := none.Match(map[string]any{"done": false})
	if err != nil || !matched {
		t.Fatalf("nil Match = %v, %v, want true", matched, err)
	}
	clause, args, err := none.SQL(1)
	if err != nil || clause != "true" || args != nil {
		t.Fatalf("nil SQL = %q, %v, %v, want true", clause, args, err)
	}
	if encoded, err := Marshal(none); err != nil || encoded != nil {
		t.Fatalf("Marshal(nil) = %v, %v, want no bytes", encoded, err)
	}
}

// TestMatch checks the matcher, including SQL's three-valued logic around null.
func TestMatch(t *testing.T) {
	t.Parallel()

	record := map[string]any{
		"title": "Rook notes",
		"name":  "notes",
		"state": "open",
		"about": "record://example/notes/01",
		"done":  true,
		"count": int64(3),
		"score": 1.5,
		"at":    testTime,
		"body":  []byte{0xa1, 0x61, 0x61, 0x01},
	}
	withNulls := map[string]any{
		"title": "Rook notes",
		"done":  nil,
		"count": nil,
	}

	tests := []struct {
		name   string
		filter string
		record map[string]any
		want   bool
	}{
		{name: "equal", filter: "done = true", record: record, want: true},
		{name: "unequal", filter: "done != true", record: record, want: false},
		{name: "contains", filter: "title contains Rook", record: record, want: true},
		{name: "contains miss", filter: "title contains rook", record: record, want: false},
		{name: "prefix", filter: "name prefix not", record: record, want: true},
		{name: "prefix miss", filter: "name prefix noted", record: record, want: false},
		{name: "greater", filter: "count > 2", record: record, want: true},
		{name: "greater equal", filter: "count >= 3", record: record, want: true},
		{name: "less", filter: "count < 3", record: record, want: false},
		{name: "float", filter: "score > 1.25", record: record, want: true},
		{name: "in", filter: "state in [open, closed]", record: record, want: true},
		{name: "in miss", filter: "state in [closed]", record: record, want: false},
		{name: "ref prefix", filter: "about prefix record://example", record: record, want: true},
		{name: "time", filter: `at >= "2024-03-01T12:00:00Z"`, record: record, want: true},
		{name: "time before", filter: `at < "2024-03-01T12:00:00Z"`, record: record, want: false},
		{name: "null test", filter: "done is null", record: withNulls, want: true},
		{name: "not null test", filter: "done is not null", record: withNulls, want: false},
		{name: "null comparison is unknown", filter: "count > 2", record: withNulls, want: false},
		{name: "unknown stays unknown through not", filter: "not count > 2", record: withNulls, want: false},
		{name: "not of unequal", filter: "not done = false", record: record, want: true},
		{name: "and", filter: "done and count > 1", record: record, want: true},
		{name: "or", filter: "not done or count > 1", record: record, want: true},
		{name: "missing field is null", filter: "name is null", record: withNulls, want: true},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()

			parsed, err := ParseText(test.filter, testFields)
			if err != nil {
				t.Fatalf("ParseText(%q): %v", test.filter, err)
			}
			matched, err := parsed.Match(test.record)
			if err != nil {
				t.Fatalf("Match: %v", err)
			}
			if matched != test.want {
				t.Errorf("%q on %v = %v, want %v", test.filter, test.record, matched, test.want)
			}
		})
	}
}

// TestSQL checks the condition and its parameters.
func TestSQL(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name     string
		filter   string
		first    int
		clause   string
		contains []any
		count    int
	}{
		{
			name:     "equal",
			filter:   "done = true",
			first:    1,
			clause:   `"done" = $1`,
			contains: []any{true},
			count:    1,
		},
		{
			name:     "parameter offset",
			filter:   "count > 2",
			first:    4,
			clause:   `"count" > $4`,
			contains: []any{int64(2)},
			count:    1,
		},
		{
			name:     "text order uses the C collation",
			filter:   "title < rook",
			first:    1,
			clause:   `("title" COLLATE "C") < $1`,
			contains: []any{"rook"},
			count:    1,
		},
		{
			name:     "contains",
			filter:   "title contains rook",
			first:    1,
			clause:   `strpos("title", $1) > 0`,
			contains: []any{"rook"},
			count:    1,
		},
		{
			name:     "prefix",
			filter:   "title prefix rook",
			first:    2,
			clause:   `starts_with("title", $2)`,
			contains: []any{"rook"},
			count:    1,
		},
		{
			name:   "in",
			filter: "count in [1, 2]",
			first:  1,
			clause: `"count" = ANY($1)`,
			count:  1,
		},
		{
			name:   "is null",
			filter: "about is null",
			clause: `"about" IS NULL`,
		},
		{
			name:   "is not null",
			filter: "about is not null",
			clause: `"about" IS NOT NULL`,
		},
		{
			name:   "not",
			filter: "not done",
			clause: `NOT ("done" = $1)`,
		},
		{
			name:   "and",
			filter: "done and count > 1",
			clause: `("done" = $1) AND ("count" > $2)`,
		},
		{
			name:   "or",
			filter: "done or count > 1",
			clause: `("done" = $1) OR ("count" > $2)`,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()

			parsed, err := ParseText(test.filter, testFields)
			if err != nil {
				t.Fatalf("ParseText(%q): %v", test.filter, err)
			}
			first := test.first
			if first == 0 {
				first = 1
			}
			clause, args, err := parsed.SQL(first)
			if err != nil {
				t.Fatalf("SQL: %v", err)
			}
			if clause != test.clause {
				t.Errorf("SQL(%q) = %q, want %q", test.filter, clause, test.clause)
			}
			if test.count > 0 && len(args) != test.count {
				t.Errorf("SQL(%q) has %d arguments, want %d", test.filter, len(args), test.count)
			}
			if len(test.contains) > 0 {
				if diff := cmp.Diff(test.contains, args); diff != "" {
					t.Errorf("arguments differ (-want +got):\n%s", diff)
				}
			}
		})
	}
}

// TestSQLInjectsNothing checks that a value with SQL in it stays a parameter.
func TestSQLInjectsNothing(t *testing.T) {
	t.Parallel()

	parsed, err := ParseText(`title = "'; DROP TABLE rookery.meta; --"`, testFields)
	if err != nil {
		t.Fatalf("ParseText: %v", err)
	}
	clause, args, err := parsed.SQL(1)
	if err != nil {
		t.Fatalf("SQL: %v", err)
	}
	if strings.Contains(clause, "DROP") {
		t.Fatalf("the clause carries the value: %q", clause)
	}
	if diff := cmp.Diff([]any{"'; DROP TABLE rookery.meta; --"}, args); diff != "" {
		t.Errorf("arguments differ (-want +got):\n%s", diff)
	}
}

// TestParseType checks the declaration's type names.
func TestParseType(t *testing.T) {
	t.Parallel()

	tests := []struct {
		text  string
		want  Type
		valid bool
	}{
		{text: "string", want: TypeString, valid: true},
		{text: "text", want: TypeText, valid: true},
		{text: "int", want: TypeInt, valid: true},
		{text: "float", want: TypeFloat, valid: true},
		{text: "bool", want: TypeBool, valid: true},
		{text: "time", want: TypeTime, valid: true},
		{text: "enum", want: TypeEnum, valid: true},
		{text: "ref", want: TypeRef, valid: true},
		{text: "vector(1536)", want: TypeVector, valid: true},
		{text: "vector", valid: false},
		{text: "vector(x)", valid: false},
		{text: "string(3)", valid: false},
		{text: "json", valid: false},
	}

	for _, test := range tests {
		t.Run(test.text, func(t *testing.T) {
			t.Parallel()

			got, err := ParseType(test.text)
			if !test.valid {
				if !errors.Is(err, ErrInvalid) {
					t.Fatalf("ParseType(%q) = %v, want an error", test.text, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("ParseType(%q): %v", test.text, err)
			}
			if got != test.want {
				t.Errorf("ParseType(%q) = %q, want %q", test.text, got, test.want)
			}
		})
	}
}

// TestConstructors checks filters built in Go rather than parsed.
func TestConstructors(t *testing.T) {
	t.Parallel()

	built := And(
		Or(Compare(OpEq, "done", true), Compare(OpNe, "count", int64(3))),
		Not(In("state", "open", "closed")),
	)
	if err := built.Validate(testFields); err != nil {
		t.Fatalf("Validate: %v", err)
	}
	if got, want := built.String(), "(done = true or count != 3) and not state in [open, closed]"; got != want {
		t.Errorf("String = %q, want %q", got, want)
	}

	// Validate normalises a time string and an integer for a float field.
	normalised := And(
		Compare(OpGe, "at", "2024-03-01T12:00:00Z"),
		Compare(OpGt, "score", int64(2)),
	)
	if err := normalised.Validate(testFields); err != nil {
		t.Fatalf("Validate: %v", err)
	}
	if _, err := normalised.Match(map[string]any{"at": testTime, "score": 3.0}); err != nil {
		t.Fatalf("Match: %v", err)
	}
	if matched, err := normalised.Match(map[string]any{"at": testTime, "score": 3.0}); err != nil || !matched {
		t.Fatalf("Match = %v, %v, want true", matched, err)
	}
}
