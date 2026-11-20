package data

import (
	"fmt"
	"math"
	"strconv"
	"strings"
	"time"

	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/filter"
)

// fieldType normalises a value to the Go type a field's column carries: string,
// int64, float64, bool, time.Time, []byte for a cbor field and []float32 for a
// vector one.
type fieldType struct {
	field Field
}

// normalise checks one value against the field and returns it in the field's Go
// type. A nil value is null and stays nil.
func (t fieldType) normalise(value any) (any, error) {
	if value == nil {
		return nil, nil
	}

	switch t.field.Type {
	case filter.TypeString, filter.TypeText:
		text, ok := value.(string)
		if !ok {
			return nil, fmt.Errorf("%s is not a string", describeValue(value))
		}
		return text, nil
	case filter.TypeEnum:
		text, ok := value.(string)
		if !ok {
			return nil, fmt.Errorf("%s is not a string", describeValue(value))
		}
		if len(t.field.Values) > 0 && !slicesContains(t.field.Values, text) {
			return nil, fmt.Errorf("%q is not one of %s", text, strings.Join(t.field.Values, ", "))
		}
		return text, nil
	case filter.TypeRef:
		text, ok := value.(string)
		if !ok {
			return nil, fmt.Errorf("%s is not a string", describeValue(value))
		}
		if !strings.Contains(text, "://") {
			return nil, fmt.Errorf("%q is not a resource URI", text)
		}
		return text, nil
	case filter.TypeInt:
		switch number := value.(type) {
		case int64:
			return number, nil
		case int:
			return int64(number), nil
		case int32:
			return int64(number), nil
		case uint64:
			if number > math.MaxInt64 {
				return nil, fmt.Errorf("%d does not fit in an int", number)
			}
			return int64(number), nil
		default:
			return nil, fmt.Errorf("%s is not an integer", describeValue(value))
		}
	case filter.TypeFloat:
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
			return nil, fmt.Errorf("%s is not a number", describeValue(value))
		}
	case filter.TypeBool:
		flag, ok := value.(bool)
		if !ok {
			return nil, fmt.Errorf("%s is not a bool", describeValue(value))
		}
		return flag, nil
	case filter.TypeTime:
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
			return nil, fmt.Errorf("%s is not a time", describeValue(value))
		}
	case filter.TypeCBOR:
		encoded, err := cborx.Marshal(value)
		if err != nil {
			return nil, fmt.Errorf("encoding %s as CBOR: %w", describeValue(value), err)
		}
		return encoded, nil
	case filter.TypeVector:
		vector, err := toVector(value)
		if err != nil {
			return nil, err
		}
		if len(vector) != t.field.Size {
			return nil, fmt.Errorf("a vector of %d dimensions is %d here", t.field.Size, len(vector))
		}
		return vector, nil
	default:
		return nil, fmt.Errorf("%q has no type", t.field.Type)
	}
}

// toVector reads a vector from the value a client sends: a list of numbers.
func toVector(value any) ([]float32, error) {
	var numbers []float32
	switch list := value.(type) {
	case []float32:
		return list, nil
	case []float64:
		numbers = make([]float32, len(list))
		for i, number := range list {
			numbers[i] = float32(number)
		}
	case []any:
		numbers = make([]float32, len(list))
		for i, item := range list {
			switch number := item.(type) {
			case float64:
				numbers[i] = float32(number)
			case float32:
				numbers[i] = number
			case int64:
				numbers[i] = float32(number)
			case uint64:
				numbers[i] = float32(number)
			default:
				return nil, fmt.Errorf("a vector holds numbers, not %s", describeValue(item))
			}
		}
	default:
		return nil, fmt.Errorf("%s is not a vector", describeValue(value))
	}
	return numbers, nil
}

// describeValue names a value's Go type for an error message.
func describeValue(value any) string {
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

// insertValues checks a record for an insert: every declared field, with its
// default where the record leaves it out, and every required field present.
func (c *Collection) insertValues(record map[string]any) (map[string]any, error) {
	if err := c.checkFields(record); err != nil {
		return nil, err
	}

	values := make(map[string]any, len(c.Fields))
	for name, field := range c.Fields {
		raw, given := record[name]
		if !given || raw == nil {
			if field.Default != nil {
				values[name] = field.Default
				continue
			}
			if field.Required {
				return nil, fmt.Errorf("%w: the field %q is required", ErrInvalid, name)
			}
			values[name] = nil
			continue
		}
		value, err := (fieldType{field: field}).normalise(raw)
		if err != nil {
			return nil, fmt.Errorf("%w: the field %q: %w", ErrInvalid, name, err)
		}
		if value == nil && field.Required {
			return nil, fmt.Errorf("%w: the field %q is required", ErrInvalid, name)
		}
		values[name] = value
	}
	return values, nil
}

// patchValues checks a record for an update: only the fields it names.
func (c *Collection) patchValues(record map[string]any) (map[string]any, error) {
	if err := c.checkFields(record); err != nil {
		return nil, err
	}

	values := make(map[string]any, len(record))
	for name, raw := range record {
		field, ok := c.Fields[name]
		if !ok {
			continue
		}
		value, err := (fieldType{field: field}).normalise(raw)
		if err != nil {
			return nil, fmt.Errorf("%w: the field %q: %w", ErrInvalid, name, err)
		}
		if value == nil && field.Required {
			return nil, fmt.Errorf("%w: the field %q is required", ErrInvalid, name)
		}
		values[name] = value
	}
	return values, nil
}

// checkFields refuses a record that names something no field is, other than the
// columns a record may carry: its id and its scope's id, which the caller may
// set.
func (c *Collection) checkFields(record map[string]any) error {
	for name := range record {
		if _, ok := c.Fields[name]; ok {
			continue
		}
		switch name {
		case "id", "space_id", "owner_id":
		default:
			return fmt.Errorf("%w: the collection %s declares no field %q", ErrInvalid, c.Key(), name)
		}
	}
	return nil
}

// vectorLiteral is a vector as Postgres reads one, so that data needs no codec
// registered on the pool for the vector type.
func vectorLiteral(vector []float32) string {
	var builder strings.Builder
	builder.WriteByte('[')
	for i, number := range vector {
		if i > 0 {
			builder.WriteByte(',')
		}
		builder.WriteString(strconv.FormatFloat(float64(number), 'f', -1, 32))
	}
	builder.WriteByte(']')
	return builder.String()
}

// parseVector reads the text form Postgres prints a vector as.
func parseVector(text string) ([]float32, error) {
	trimmed := strings.TrimSuffix(strings.TrimPrefix(text, "["), "]")
	if trimmed == "" {
		return []float32{}, nil
	}
	parts := strings.Split(trimmed, ",")
	vector := make([]float32, len(parts))
	for i, part := range parts {
		number, err := strconv.ParseFloat(strings.TrimSpace(part), 32)
		if err != nil {
			return nil, fmt.Errorf("data: %q is not a vector: %w", text, err)
		}
		vector[i] = float32(number)
	}
	return vector, nil
}

// slicesContains reports whether a list holds a value.
func slicesContains(list []string, value string) bool {
	for _, item := range list {
		if item == value {
			return true
		}
	}
	return false
}

// columnValue turns a value a row was scanned with into the record's value.
func (c *Collection) columnValue(name string, raw any) (any, error) {
	field, ok := c.Fields[name]
	if !ok {
		// A system column: id, rev, the timestamps and the scope's id.
		return raw, nil
	}
	if raw == nil {
		return nil, nil
	}
	if field.Type == filter.TypeVector {
		text, ok := raw.(string)
		if !ok {
			return nil, fmt.Errorf("data: the vector column %q came back as %T", name, raw)
		}
		return parseVector(text)
	}
	return raw, nil
}
