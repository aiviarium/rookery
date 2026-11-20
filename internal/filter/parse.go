package filter

import (
	"fmt"
	"sort"
	"strings"

	"github.com/aiviarium/rookery/internal/cborx"
)

// Parse decodes a filter from its CBOR structure and validates it against the
// collection's fields. Empty data is no filter: the result is nil, which matches
// everything.
//
// The structure is documented on the package: comparisons as maps with field, op
// and value, and combinations as maps with and, or or not. A top-level array is
// an and of its elements.
func Parse(data []byte, fields Fields) (*Filter, error) {
	if len(data) == 0 {
		return nil, nil
	}

	var decoded any
	if err := cborx.Unmarshal(data, &decoded); err != nil {
		return nil, fmt.Errorf("%w: %w", ErrInvalid, err)
	}

	parsed, err := parseStructure(decoded)
	if err != nil {
		return nil, err
	}
	if err := parsed.Validate(fields); err != nil {
		return nil, err
	}
	return parsed, nil
}

// Marshal returns the CBOR structure of a filter, as Parse takes it. A nil filter
// encodes to no bytes.
func Marshal(f *Filter) ([]byte, error) {
	if f == nil {
		return nil, nil
	}
	return cborx.Marshal(structure(f))
}

// structure renders a filter as the CBOR structure.
func structure(f *Filter) any {
	switch f.op {
	case OpAnd, OpOr:
		children := make([]any, len(f.children))
		for i, child := range f.children {
			children[i] = structure(child)
		}
		return map[string]any{string(f.op): children}
	case OpNot:
		return map[string]any{"not": structure(f.children[0])}
	case OpIn:
		return map[string]any{"field": f.field, "op": string(f.op), "value": f.values}
	default:
		return map[string]any{"field": f.field, "op": string(f.op), "value": f.value}
	}
}

// parseStructure reads one node of the CBOR structure.
func parseStructure(node any) (*Filter, error) {
	switch node := node.(type) {
	case []any:
		if len(node) == 0 {
			return nil, fmt.Errorf("%w: an empty array is not a filter", ErrInvalid)
		}
		children := make([]*Filter, len(node))
		for i, element := range node {
			child, err := parseStructure(element)
			if err != nil {
				return nil, err
			}
			children[i] = child
		}
		return And(children...), nil
	case map[string]any:
		return parseMap(node)
	case map[any]any:
		fields := make(map[string]any, len(node))
		for key, value := range node {
			name, ok := key.(string)
			if !ok {
				return nil, fmt.Errorf("%w: a key of a filter is not a string", ErrInvalid)
			}
			fields[name] = value
		}
		return parseMap(fields)
	default:
		return nil, fmt.Errorf("%w: %s is not a filter", ErrInvalid, describe(node))
	}
}

// parseMap reads one map of the CBOR structure: a combination or a comparison.
func parseMap(node map[string]any) (*Filter, error) {
	var combinations []string
	for _, name := range []string{"and", "or", "not"} {
		if _, ok := node[name]; ok {
			combinations = append(combinations, name)
		}
	}
	if len(combinations) > 0 {
		if len(combinations) > 1 {
			sort.Strings(combinations)
			return nil, fmt.Errorf("%w: a filter combines with one of %s, not %s",
				ErrInvalid, strings.Join(combinations, ", "), strings.Join(combinations, " and "))
		}
		if len(node) != 1 {
			return nil, fmt.Errorf("%w: %q takes no other keys", ErrInvalid, combinations[0])
		}
		return parseCombination(combinations[0], node[combinations[0]])
	}

	for key := range node {
		switch key {
		case "field", "op", "value":
		default:
			return nil, fmt.Errorf("%w: %q is not a key of a comparison", ErrInvalid, key)
		}
	}

	rawField, ok := node["field"]
	if !ok {
		return nil, fmt.Errorf("%w: a map is a comparison with a field or a combination with and, or or not", ErrInvalid)
	}
	field, ok := rawField.(string)
	if !ok || field == "" {
		return nil, fmt.Errorf("%w: field is not a name", ErrInvalid)
	}

	op := OpEq
	if rawOp, ok := node["op"]; ok {
		text, isText := rawOp.(string)
		if !isText {
			return nil, fmt.Errorf("%w: op is not a string", ErrInvalid)
		}
		parsed, err := ParseOp(text)
		if err != nil {
			return nil, err
		}
		op = parsed
	}

	value, hasValue := node["value"]
	if !hasValue {
		return nil, fmt.Errorf("%w: the comparison on %q has no value", ErrInvalid, field)
	}

	if op == OpIn {
		list, ok := value.([]any)
		if !ok {
			return nil, fmt.Errorf("%w: in on %q takes a list", ErrInvalid, field)
		}
		return In(field, list...), nil
	}
	return Compare(op, field, value), nil
}

// parseCombination reads and, or and not.
func parseCombination(name string, node any) (*Filter, error) {
	if name == "not" {
		child, err := parseStructure(node)
		if err != nil {
			return nil, err
		}
		return Not(child), nil
	}

	list, ok := node.([]any)
	if !ok {
		return nil, fmt.Errorf("%w: %q takes a list of filters", ErrInvalid, name)
	}
	if len(list) == 0 {
		return nil, fmt.Errorf("%w: %q takes at least one filter", ErrInvalid, name)
	}
	children := make([]*Filter, len(list))
	for i, element := range list {
		child, err := parseStructure(element)
		if err != nil {
			return nil, err
		}
		children[i] = child
	}
	if name == "and" {
		return And(children...), nil
	}
	return Or(children...), nil
}
