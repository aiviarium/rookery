// Package hostaction defines the action arguments shared by the server and daemon.
package hostaction

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"

	"github.com/aiviarium/rookery/internal/cborx"
)

// Argument describes one argument to a declared action.
type Argument struct {
	Name     string `json:"name"`
	Label    string `json:"label,omitempty"`
	Required bool   `json:"required,omitempty"`
	Default  string `json:"default,omitempty"`
}

// Environment validates CBOR arguments and translates them into variables.
func Environment(arguments []Argument, encoded []byte) (map[string]string, error) {
	var given map[string]any
	if len(encoded) > 0 {
		if err := cborx.Unmarshal(encoded, &given); err != nil {
			return nil, err
		}
	}

	env := make(map[string]string, len(arguments))
	declared := make(map[string]bool, len(arguments))
	for _, argument := range arguments {
		name, err := Variable(argument.Name)
		if err != nil {
			return nil, err
		}
		if _, duplicate := env[name]; duplicate {
			return nil, fmt.Errorf("arguments map to the same variable %s", name)
		}
		declared[argument.Name] = true
		value := argument.Default
		if input, ok := given[argument.Name]; ok {
			value, err = format(input)
			if err != nil {
				return nil, fmt.Errorf("argument %s: %w", argument.Name, err)
			}
		}
		if strings.ContainsRune(value, '\x00') {
			return nil, fmt.Errorf("argument %s contains NUL", argument.Name)
		}
		if argument.Required && value == "" {
			return nil, fmt.Errorf("argument %s is required", argument.Name)
		}
		env[name] = value
	}
	for name := range given {
		if !declared[name] {
			return nil, fmt.Errorf("unknown argument %q", name)
		}
	}
	return env, nil
}

// Variable maps an argument name to the environment key available in its command.
func Variable(name string) (string, error) {
	if name == "" {
		return "", errors.New("an argument needs a name")
	}
	for _, char := range name {
		if (char < 'a' || char > 'z') && (char < 'A' || char > 'Z') && (char < '0' || char > '9') && char != '_' && char != '-' {
			return "", fmt.Errorf("invalid argument name %q", name)
		}
	}
	return "ROOKERY_ARG_" + strings.ToUpper(strings.ReplaceAll(name, "-", "_")), nil
}

func format(value any) (string, error) {
	switch value := value.(type) {
	case nil:
		return "", nil
	case string:
		return value, nil
	case []byte:
		return string(value), nil
	default:
		encoded, err := json.Marshal(cborx.JSONValue(value))
		return string(encoded), err
	}
}
