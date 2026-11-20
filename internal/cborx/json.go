package cborx

import (
	"encoding/json"
	"fmt"
	"strings"
	"time"
)

// ParseText accepts a JSON value, or treats other text as a string.
func ParseText(raw string) any {
	decoder := json.NewDecoder(strings.NewReader(raw))
	decoder.UseNumber()
	var value any
	if !json.Valid([]byte(raw)) {
		return raw
	}
	if err := decoder.Decode(&value); err != nil {
		return raw
	}
	return numbersToGo(value)
}

// numbersToGo turns JSON's numbers into the Go types CBOR has: integers stay
// integers.
func numbersToGo(value any) any {
	switch typed := value.(type) {
	case json.Number:
		if integer, err := typed.Int64(); err == nil {
			return integer
		}
		if number, err := typed.Float64(); err == nil {
			return number
		}
		return typed.String()
	case []any:
		for i, item := range typed {
			typed[i] = numbersToGo(item)
		}
		return typed
	case map[string]any:
		for key, item := range typed {
			typed[key] = numbersToGo(item)
		}
		return typed
	default:
		return value
	}
}

// JSONValue turns a decoded CBOR value into one JSON can write: CBOR's maps have
// keys of any type and JSON's are strings, and times become RFC 3339.
func JSONValue(value any) any {
	switch typed := value.(type) {
	case map[any]any:
		object := make(map[string]any, len(typed))
		for key, item := range typed {
			object[fmt.Sprint(key)] = JSONValue(item)
		}
		return object
	case map[string]any:
		for key, item := range typed {
			typed[key] = JSONValue(item)
		}
		return typed
	case []any:
		for i, item := range typed {
			typed[i] = JSONValue(item)
		}
		return typed
	case time.Time:
		return typed.UTC().Format(time.RFC3339Nano)
	default:
		return value
	}
}
