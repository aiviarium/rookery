// Package filter is the datastore's filter language.
//
// A filter compares declared fields and combines the comparisons with and, or
// and not. It arrives as the CBOR structure a client sends, or as the text form
// the CLI takes, and is validated against the collection's declared fields. A
// validated filter either becomes a SQL condition with parameters, which the
// datastore puts in a query, or matches a record in memory, which live queries
// and tests do. Both agree on every filter and record: a differential test runs
// random filters through Postgres and through the matcher and compares.
//
// The CBOR structure is a map with exactly one of
//
//	{"field": "done", "op": "eq", "value": false}
//	{"field": "title", "op": "contains", "value": "rook"}
//	{"field": "size", "op": "in", "value": [1, 2, 3]}
//	{"and": [ … ]}, {"or": [ … ]}, {"not": …}
//
// with "op" defaulting to "eq" when a value is given. A top-level array is an
// "and" of its elements. A null value means the column is null: "eq" is IS NULL
// and "ne" is IS NOT NULL.
//
// The text form is what `rookery data find … --where` takes:
//
//	title contains rook and not done
//	state in [open, closed] or owner != "user:someone"
//	finished is null
//
// with the operators = == != <> < <= > >= in contains prefix eq ne lt le gt ge,
// the keywords and, or, not, the literals true, false, null, quoted strings and
// [ … ] lists. A bare word is a string, so enum and ref values need no quotes.
//
// Null is three-valued, as in SQL: a comparison with a null column is unknown,
// not(unknown) is unknown, and only true matches. That is what makes the SQL and
// the matcher agree.
package filter

import (
	"errors"
	"fmt"
	"strconv"
	"strings"
)

// Type is a declared field's type, as a collection declaration writes it.
type Type string

// The declared field types. Vector is the one type a filter cannot name: a
// similar-to search is the entity search's job, not a condition.
const (
	TypeString Type = "string"
	TypeText   Type = "text"
	TypeInt    Type = "int"
	TypeFloat  Type = "float"
	TypeBool   Type = "bool"
	TypeTime   Type = "time"
	TypeEnum   Type = "enum"
	TypeRef    Type = "ref"
	TypeVector Type = "vector"
	TypeCBOR   Type = "cbor"
)

// ParseType reads a field type as a declaration writes it. "vector(N)" is a
// vector of N dimensions; the size belongs to the declaration and the filter
// language treats every vector alike.
func ParseType(text string) (Type, error) {
	base, size, hasSize := strings.Cut(strings.ToLower(strings.TrimSpace(text)), "(")
	base = strings.TrimSpace(base)
	if hasSize {
		if !strings.HasSuffix(size, ")") {
			return "", fmt.Errorf("%w: the field type %q is malformed", ErrInvalid, text)
		}
		digits := strings.TrimSuffix(size, ")")
		if _, err := strconv.Atoi(digits); err != nil {
			return "", fmt.Errorf("%w: the vector size in %q is not a number", ErrInvalid, text)
		}
	}
	switch Type(base) {
	case TypeString, TypeText, TypeInt, TypeFloat, TypeBool, TypeTime, TypeEnum, TypeRef, TypeCBOR:
		if hasSize {
			return "", fmt.Errorf("%w: the field type %q takes no size", ErrInvalid, text)
		}
		return Type(base), nil
	case TypeVector:
		if !hasSize {
			return "", fmt.Errorf("%w: a vector field needs its size, as vector(1536)", ErrInvalid)
		}
		return TypeVector, nil
	default:
		return "", fmt.Errorf("%w: %q is not a field type", ErrInvalid, base)
	}
}

// Fields are the fields a filter may name, with their types.
type Fields map[string]Type

// filterable reports whether any operator applies to the type.
func (t Type) filterable() bool { return t != TypeVector && t != "" }

// Op is a filter operator.
type Op string

// The operators. And, or and not take children; the rest compare one field
// with a value.
const (
	OpEq       Op = "eq"
	OpNe       Op = "ne"
	OpLt       Op = "lt"
	OpLe       Op = "le"
	OpGt       Op = "gt"
	OpGe       Op = "ge"
	OpIn       Op = "in"
	OpContains Op = "contains"
	OpPrefix   Op = "prefix"
	OpAnd      Op = "and"
	OpOr       Op = "or"
	OpNot      Op = "not"
)

// ParseOp reads an operator as the text form or the CBOR form writes it.
func ParseOp(text string) (Op, error) {
	switch strings.ToLower(strings.TrimSpace(text)) {
	case "eq", "=", "==":
		return OpEq, nil
	case "ne", "!=", "<>":
		return OpNe, nil
	case "lt", "<":
		return OpLt, nil
	case "le", "<=":
		return OpLe, nil
	case "gt", ">":
		return OpGt, nil
	case "ge", ">=":
		return OpGe, nil
	case "in":
		return OpIn, nil
	case "contains":
		return OpContains, nil
	case "prefix":
		return OpPrefix, nil
	case "and":
		return OpAnd, nil
	case "or":
		return OpOr, nil
	case "not":
		return OpNot, nil
	default:
		return "", fmt.Errorf("%w: %q is not an operator", ErrInvalid, text)
	}
}

// The errors a filter reports. They wrap one of these, so a caller checks with
// errors.Is and reads the detail from the message.
var (
	// ErrInvalid is a filter that does not parse.
	ErrInvalid = errors.New("filter: invalid filter")

	// ErrUnknownField is a filter that names a field the collection does not
	// declare.
	ErrUnknownField = errors.New("filter: unknown field")

	// ErrUnsupported is an operator a field's type does not take, or a value
	// that is not of the field's type.
	ErrUnsupported = errors.New("filter: unsupported")

	// ErrUnvalidated is a filter used before it was checked against its
	// fields.
	ErrUnvalidated = errors.New("filter: the filter has not been validated")
)

// Filter is a parsed filter expression. A nil *Filter matches every record and
// means no filter at all.
//
// A filter must be validated before Match or SQL: Parse, ParseText and Validate
// do that. Validation normalises values to the Go types a record carries, so a
// filter is not safe to validate from two goroutines at once, and a validated
// one is read-only.
type Filter struct {
	op        Op
	field     string
	fieldType Type
	value     any
	values    []any
	children  []*Filter
	validated bool
}
