package filter

import (
	"fmt"
	"strconv"
	"strings"
	"time"

	"github.com/aiviarium/rookery/internal/cborx"
)

// Compare returns a comparison of a field with a value. A null value means the
// column is null.
func Compare(op Op, field string, value any) *Filter {
	return &Filter{op: op, field: field, value: value}
}

// In returns a membership test.
func In(field string, values ...any) *Filter {
	return &Filter{op: OpIn, field: field, values: values}
}

// And returns a filter that matches when every child does. No children matches
// everything.
func And(children ...*Filter) *Filter {
	return &Filter{op: OpAnd, children: children}
}

// Or returns a filter that matches when any child does. No children matches
// nothing.
func Or(children ...*Filter) *Filter {
	return &Filter{op: OpOr, children: children}
}

// Not returns a filter that matches when its child does not.
func Not(child *Filter) *Filter {
	return &Filter{op: OpNot, children: []*Filter{child}}
}

// Validate checks a filter against the declared fields and normalises its
// values, and reports the first thing that is wrong. A nil filter is valid and
// matches everything. Validating a filter that is already valid does nothing.
func (f *Filter) Validate(fields Fields) error {
	if f == nil || f.validated {
		return nil
	}

	switch f.op {
	case OpAnd, OpOr:
		for _, child := range f.children {
			if err := child.Validate(fields); err != nil {
				return err
			}
		}
		f.validated = true
		return nil
	case OpNot:
		if len(f.children) != 1 {
			return fmt.Errorf("%w: not takes one filter", ErrInvalid)
		}
		if err := f.children[0].Validate(fields); err != nil {
			return err
		}
		f.validated = true
		return nil
	case OpEq, OpNe, OpLt, OpLe, OpGt, OpGe, OpIn, OpContains, OpPrefix:
	default:
		return fmt.Errorf("%w: %q is not an operator", ErrInvalid, f.op)
	}

	if f.field == "" {
		return fmt.Errorf("%w: a comparison needs a field", ErrInvalid)
	}
	typ, ok := fields[f.field]
	if !ok {
		return fmt.Errorf("%w: the collection declares no field %q", ErrUnknownField, f.field)
	}
	if !typ.filterable() {
		return fmt.Errorf("%w: a %s field cannot be filtered on", ErrUnsupported, typ)
	}
	f.fieldType = typ

	if err := f.checkOp(); err != nil {
		return err
	}

	if f.op == OpIn {
		if len(f.values) == 0 {
			return fmt.Errorf("%w: in takes at least one value", ErrInvalid)
		}
		normalised := make([]any, len(f.values))
		for i, raw := range f.values {
			value, err := normalizeValue(typ, raw)
			if err != nil {
				return fmt.Errorf("%w: %s: %w", ErrUnsupported, f.field, err)
			}
			if value == nil {
				return fmt.Errorf("%w: %s: in takes no null", ErrUnsupported, f.field)
			}
			normalised[i] = value
		}
		f.values = normalised
		f.validated = true
		return nil
	}

	value, err := normalizeValue(typ, f.value)
	if err != nil {
		return fmt.Errorf("%w: %s: %w", ErrUnsupported, f.field, err)
	}
	f.value = value
	f.validated = true
	return nil
}

// checkOp reports whether the operator applies to the field's type.
func (f *Filter) checkOp() error {
	switch f.op {
	case OpEq, OpNe:
		return nil
	case OpIn:
		return nil
	case OpContains, OpPrefix:
		if f.fieldType != TypeString && f.fieldType != TypeText && f.fieldType != TypeRef {
			return fmt.Errorf("%w: %s takes no %s", ErrUnsupported, f.fieldType, f.op)
		}
		return nil
	case OpLt, OpLe, OpGt, OpGe:
		switch f.fieldType {
		case TypeInt, TypeFloat, TypeTime, TypeString, TypeText:
		default:
			return fmt.Errorf("%w: %s takes no %s order", ErrUnsupported, f.fieldType, f.op)
		}
		if f.value == nil {
			return fmt.Errorf("%w: a comparison with null has no order", ErrUnsupported)
		}
		return nil
	default:
		return fmt.Errorf("%w: %q is not an operator", ErrInvalid, f.op)
	}
}

// normalizeValue turns a value from a request into the Go type a record carries
// for the field: int64, float64, string, bool, time.Time or, for a cbor field,
// its deterministic encoding.
func normalizeValue(typ Type, value any) (any, error) {
	if value == nil {
		return nil, nil
	}

	switch typ {
	case TypeString, TypeText, TypeEnum:
		text, ok := value.(string)
		if !ok {
			return nil, fmt.Errorf("%s is not a string", describe(value))
		}
		return text, nil
	case TypeRef:
		text, ok := value.(string)
		if !ok {
			return nil, fmt.Errorf("%s is not a string", describe(value))
		}
		if !strings.Contains(text, "://") {
			return nil, fmt.Errorf("%q is not a resource URI", text)
		}
		return text, nil
	case TypeInt:
		switch number := value.(type) {
		case int64:
			return number, nil
		case int:
			return int64(number), nil
		case int32:
			return int64(number), nil
		case uint64:
			if number > 1<<63-1 {
				return nil, fmt.Errorf("%d does not fit in an int", number)
			}
			return int64(number), nil
		default:
			return nil, fmt.Errorf("%s is not an integer", describe(value))
		}
	case TypeFloat:
		switch number := value.(type) {
		case float64:
			return number, nil
		case float32:
			return float64(number), nil
		case int64:
			return float64(number), nil
		case int:
			return float64(number), nil
		case uint64:
			return float64(number), nil
		default:
			return nil, fmt.Errorf("%s is not a number", describe(value))
		}
	case TypeBool:
		flag, ok := value.(bool)
		if !ok {
			return nil, fmt.Errorf("%s is not a bool", describe(value))
		}
		return flag, nil
	case TypeTime:
		switch moment := value.(type) {
		case time.Time:
			return moment.UTC(), nil
		case string:
			parsed, err := time.Parse(time.RFC3339Nano, moment)
			if err != nil {
				return nil, fmt.Errorf("%q is not a time in RFC 3339", moment)
			}
			return parsed.UTC(), nil
		default:
			return nil, fmt.Errorf("%s is not a time", describe(value))
		}
	case TypeCBOR:
		encoded, err := cborx.Marshal(value)
		if err != nil {
			return nil, fmt.Errorf("encoding %s as CBOR: %w", describe(value), err)
		}
		return encoded, nil
	default:
		return nil, fmt.Errorf("%s fields cannot be filtered on", typ)
	}
}

// describe names a value's Go type for an error message.
func describe(value any) string {
	switch value.(type) {
	case nil:
		return "null"
	case bool:
		return "a bool"
	case int, int32, int64, uint64:
		return "an integer"
	case float32, float64:
		return "a number"
	case string:
		return "a string"
	case time.Time:
		return "a time"
	case []byte:
		return "bytes"
	case []any, []string, []int64, []float64, []bool, []time.Time:
		return "a list"
	default:
		return fmt.Sprintf("a %T", value)
	}
}

// String renders the text form of a filter, which the CLI prints and tests
// compare.
func (f *Filter) String() string {
	if f == nil {
		return ""
	}
	switch f.op {
	case OpAnd:
		parts := make([]string, len(f.children))
		for i, child := range f.children {
			parts[i] = child.operand()
		}
		if len(parts) == 0 {
			return "true"
		}
		return strings.Join(parts, " and ")
	case OpOr:
		parts := make([]string, len(f.children))
		for i, child := range f.children {
			parts[i] = child.operand()
		}
		if len(parts) == 0 {
			return "false"
		}
		return strings.Join(parts, " or ")
	case OpNot:
		return "not " + f.children[0].operand()
	case OpIn:
		values := make([]string, len(f.values))
		for i, value := range f.values {
			values[i] = formatValue(value)
		}
		return f.field + " in [" + strings.Join(values, ", ") + "]"
	case OpEq:
		if f.value == nil {
			return f.field + " is null"
		}
		return f.field + " = " + formatValue(f.value)
	case OpNe:
		if f.value == nil {
			return f.field + " is not null"
		}
		return f.field + " != " + formatValue(f.value)
	case OpContains:
		return f.field + " contains " + formatValue(f.value)
	case OpPrefix:
		return f.field + " prefix " + formatValue(f.value)
	default:
		return f.field + " " + string(f.op) + " " + formatValue(f.value)
	}
}

// operand renders a child of and, or or not, in parentheses when it is itself a
// combination.
func (f *Filter) operand() string {
	text := f.String()
	switch f.op {
	case OpAnd, OpOr:
		return "(" + text + ")"
	default:
		return text
	}
}

// formatValue renders a value in the text form.
func formatValue(value any) string {
	switch value := value.(type) {
	case nil:
		return "null"
	case bool:
		return strconv.FormatBool(value)
	case int64:
		return strconv.FormatInt(value, 10)
	case float64:
		return strconv.FormatFloat(value, 'g', -1, 64)
	case string:
		if isBareWord(value) {
			return value
		}
		return strconv.Quote(value)
	case time.Time:
		return strconv.Quote(value.UTC().Format(time.RFC3339Nano))
	case []byte:
		return strconv.Quote(string(value))
	default:
		return fmt.Sprint(value)
	}
}

// isBareWord reports whether a string can stand unquoted in the text form: a
// word that no keyword, operator or number could be mistaken for.
func isBareWord(text string) bool {
	if text == "" {
		return false
	}
	for i, r := range text {
		switch {
		case r >= 'a' && r <= 'z', r >= 'A' && r <= 'Z', r == '_':
		case i > 0 && (r >= '0' && r <= '9' || r == '-' || r == '.' || r == ':' || r == '/'):
		default:
			return false
		}
	}
	switch strings.ToLower(text) {
	case "and", "or", "not", "in", "contains", "prefix", "is", "null", "true", "false":
		return false
	}
	return true
}
