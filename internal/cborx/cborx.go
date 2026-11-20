// Package cborx encodes and decodes the dynamic content in Rookery's bytes
// fields and bytea columns: datastore records, feature variables, provisioner
// arguments and state, action and job arguments, settings values, kv values, and
// payloads.
//
// Values are CBOR in Core Deterministic mode (RFC 8949 section 4.2.1): map keys
// are sorted bytewise, lengths are definite and floats take the shortest form
// that keeps their value. Times are CBOR tag 0 text in RFC 3339 with
// nanoseconds, in UTC. Two encodings of one value are therefore the same bytes,
// which the applied digest and the change feed rely on.
package cborx

import (
	"fmt"
	"sync"

	"github.com/fxamacker/cbor/v2"
)

// MapPairs bounds the number of pairs a decoded map may have, and ArrayItems the
// number of a decoded array, so a hostile or broken payload cannot make the
// server allocate without limit.
const (
	MapPairs   = 1 << 20
	ArrayItems = 1 << 20

	// Depth is the number of array, map and tag levels a value may nest.
	Depth = 32
)

// Marshal returns the Core Deterministic CBOR encoding of v.
func Marshal(v any) ([]byte, error) {
	mode, err := encMode()
	if err != nil {
		return nil, err
	}
	encoded, err := mode.Marshal(v)
	if err != nil {
		return nil, fmt.Errorf("cborx: encoding %T: %w", v, err)
	}
	return encoded, nil
}

// Unmarshal decodes the Core Deterministic CBOR in data into v.
//
// It refuses duplicate map keys, indefinite lengths, invalid UTF-8 and nesting
// deeper than Depth, so that one value has exactly one accepted encoding. Fields
// v does not have are ignored, so a newer writer's extra fields do not break an
// older reader.
func Unmarshal(data []byte, v any) error {
	mode, err := decMode()
	if err != nil {
		return err
	}
	if err := mode.Unmarshal(data, v); err != nil {
		return fmt.Errorf("cborx: decoding into %T: %w", v, err)
	}
	return nil
}

// The modes are built once, on first use, and are immutable and safe for
// concurrent use.
var (
	encMode = sync.OnceValues(newEncoding)
	decMode = sync.OnceValues(newDecoding)
)

func newEncoding() (cbor.EncMode, error) {
	options := cbor.CoreDetEncOptions()
	// Times are CBOR tag 0 text in RFC 3339 with nanoseconds, in UTC. Epoch
	// seconds are a float and cannot keep nanoseconds, and Rookery's times come
	// from Postgres, which keeps microseconds.
	options.Time = cbor.TimeRFC3339NanoUTC
	mode, err := options.EncMode()
	if err != nil {
		return nil, fmt.Errorf("cborx: building the encoding mode: %w", err)
	}
	return mode, nil
}

func newDecoding() (cbor.DecMode, error) {
	options := cbor.DecOptions{
		DupMapKey:        cbor.DupMapKeyEnforcedAPF,
		IndefLength:      cbor.IndefLengthForbidden,
		TimeTag:          cbor.DecTagOptional,
		IntDec:           cbor.IntDecConvertSignedOrFail,
		MaxNestedLevels:  Depth,
		MaxArrayElements: ArrayItems,
		MaxMapPairs:      MapPairs,
	}
	mode, err := options.DecMode()
	if err != nil {
		return nil, fmt.Errorf("cborx: building the decoding mode: %w", err)
	}
	return mode, nil
}
