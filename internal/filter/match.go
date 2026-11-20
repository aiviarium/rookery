package filter

import (
	"bytes"
	"strings"
	"time"
)

// truth is a filter's result against a record: SQL's three-valued logic, where
// a comparison with a null column is unknown and only true matches.
type truth uint8

const (
	truthUnknown truth = iota
	truthFalse
	truthTrue
)

// Match reports whether a record matches the filter. A nil filter matches
// everything. The record holds a value per declared field, in the Go type the
// field's declaration carries: int64, float64, string, bool, time.Time, []byte
// for a cbor field, and nil for null. A field that is absent is null.
//
// The filter must be validated first.
func (f *Filter) Match(record map[string]any) (bool, error) {
	if f == nil {
		return true, nil
	}
	if !f.validated {
		return false, ErrUnvalidated
	}
	return f.eval(record) == truthTrue, nil
}

// eval returns the filter's three-valued result.
func (f *Filter) eval(record map[string]any) truth {
	switch f.op {
	case OpAnd:
		result := truthTrue
		for _, child := range f.children {
			switch child.eval(record) {
			case truthFalse:
				return truthFalse
			case truthUnknown:
				result = truthUnknown
			}
		}
		return result
	case OpOr:
		result := truthFalse
		for _, child := range f.children {
			switch child.eval(record) {
			case truthTrue:
				return truthTrue
			case truthUnknown:
				result = truthUnknown
			}
		}
		return result
	case OpNot:
		switch f.children[0].eval(record) {
		case truthTrue:
			return truthFalse
		case truthFalse:
			return truthTrue
		default:
			return truthUnknown
		}
	default:
		return f.compare(record[f.field])
	}
}

// compare evaluates a comparison against the record's value for its field.
func (f *Filter) compare(value any) truth {
	if f.op == OpEq && f.value == nil {
		return truthOf(value == nil)
	}
	if f.op == OpNe && f.value == nil {
		return truthOf(value != nil)
	}
	if value == nil {
		// Every comparison with a null column is unknown, as in SQL.
		return truthUnknown
	}

	switch f.op {
	case OpEq:
		return truthOf(equalValues(value, f.value))
	case OpNe:
		return truthOf(!equalValues(value, f.value))
	case OpIn:
		for _, candidate := range f.values {
			if equalValues(value, candidate) {
				return truthTrue
			}
		}
		return truthFalse
	case OpLt, OpLe, OpGt, OpGe:
		order, ok := compareValues(value, f.value)
		if !ok {
			return truthUnknown
		}
		switch f.op {
		case OpLt:
			return truthOf(order < 0)
		case OpLe:
			return truthOf(order <= 0)
		case OpGt:
			return truthOf(order > 0)
		default:
			return truthOf(order >= 0)
		}
	case OpContains:
		text, isText := value.(string)
		needle, _ := f.value.(string)
		if !isText {
			return truthUnknown
		}
		return truthOf(strings.Contains(text, needle))
	case OpPrefix:
		text, isText := value.(string)
		prefix, _ := f.value.(string)
		if !isText {
			return truthUnknown
		}
		return truthOf(strings.HasPrefix(text, prefix))
	default:
		return truthUnknown
	}
}

func truthOf(flag bool) truth {
	if flag {
		return truthTrue
	}
	return truthFalse
}

// equalValues reports whether two non-null values are equal. Values of the same
// field type compare; a value of another type is not equal to anything.
func equalValues(a, b any) bool {
	if a == nil || b == nil {
		return a == nil && b == nil
	}

	switch left := a.(type) {
	case string:
		right, ok := b.(string)
		return ok && left == right
	case bool:
		right, ok := b.(bool)
		return ok && left == right
	case time.Time:
		right, ok := b.(time.Time)
		return ok && left.Equal(right)
	case []byte:
		right, ok := b.([]byte)
		return ok && bytes.Equal(left, right)
	}

	if left, ok := asInt(a); ok {
		if right, ok := asInt(b); ok {
			return left == right
		}
		right, ok := asFloat(b)
		return ok && float64(left) == right
	}
	if left, ok := asFloat(a); ok {
		right, ok := asFloat(b)
		return ok && left == right
	}
	return false
}

// compareValues orders two non-null values of the same field type. ok is false
// when they are not comparable, which in a valid filter means a record that does
// not hold the declared type.
func compareValues(a, b any) (int, bool) {
	switch left := a.(type) {
	case string:
		right, ok := b.(string)
		if !ok {
			return 0, false
		}
		return strings.Compare(left, right), true
	case time.Time:
		right, ok := b.(time.Time)
		if !ok {
			return 0, false
		}
		switch {
		case left.Before(right):
			return -1, true
		case left.After(right):
			return 1, true
		default:
			return 0, true
		}
	}

	if left, ok := asInt(a); ok {
		if right, ok := asInt(b); ok {
			switch {
			case left < right:
				return -1, true
			case left > right:
				return 1, true
			default:
				return 0, true
			}
		}
	}
	left, leftOK := asFloat(a)
	right, rightOK := asFloat(b)
	if !leftOK || !rightOK {
		return 0, false
	}
	switch {
	case left < right:
		return -1, true
	case left > right:
		return 1, true
	default:
		return 0, true
	}
}

// asInt reads a value as an integer, when it is one.
func asInt(value any) (int64, bool) {
	switch number := value.(type) {
	case int:
		return int64(number), true
	case int32:
		return int64(number), true
	case int64:
		return number, true
	case uint64:
		if number > 1<<63-1 {
			return 0, false
		}
		return int64(number), true
	default:
		return 0, false
	}
}

// asFloat reads a value as a float, when it is a number of any kind.
func asFloat(value any) (float64, bool) {
	switch number := value.(type) {
	case float32:
		return float64(number), true
	case float64:
		return number, true
	default:
		if whole, ok := asInt(value); ok {
			return float64(whole), true
		}
		return 0, false
	}
}
