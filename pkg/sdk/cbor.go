package sdk

import (
	"github.com/aiviarium/rookery/internal/cborx"
)

// Marshal returns the Core Deterministic CBOR encoding of a value, for one of the
// bytes fields that carry dynamic content: arguments, state, a record.
//
// It is the same encoding the core uses, so a value the SDK wrote is a value the
// core reads, byte for byte.
func Marshal(v any) ([]byte, error) { return cborx.Marshal(v) }

// Unmarshal decodes Core Deterministic CBOR into a value.
func Unmarshal(data []byte, v any) error { return cborx.Unmarshal(data, v) }
