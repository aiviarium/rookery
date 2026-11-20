// Package id makes the identifiers rows are named by, and the secrets that
// name a session or a token.
//
// An id is a ULID: 48 bits of milliseconds since the epoch and 80 random bits,
// in Crockford's base32. Ids sort in the order they were made, so a paged list
// takes the last id it saw as its cursor.
package id

import (
	"crypto/rand"
	"encoding/base32"
	"fmt"
	"math/big"
	"strings"
	"time"
)

// Length is how many characters an id has.
const Length = 26

// alphabet is Crockford's base32: no I, L, O or U, so an id typed by a person
// is not ambiguous.
const alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"

// New returns a new id.
func New() string {
	var raw [16]byte
	millis := uint64(time.Now().UnixMilli())
	for i := 5; i >= 0; i-- {
		raw[i] = byte(millis)
		millis >>= 8
	}
	// The read cannot fail: crypto/rand.Read only fails when the platform has
	// no source of randomness at all, and then nothing here is safe.
	if _, err := rand.Read(raw[6:]); err != nil {
		panic(fmt.Sprintf("id: reading random bytes: %v", err))
	}

	// 128 bits become 26 characters of 5 bits, the top two of them zero.
	number := new(big.Int).SetBytes(raw[:])
	mask := big.NewInt(31)
	out := make([]byte, Length)
	for i := Length - 1; i >= 0; i-- {
		out[i] = alphabet[new(big.Int).And(number, mask).Int64()]
		number.Rsh(number, 5)
	}
	return string(out)
}

// Valid reports whether s is an id this package made.
func Valid(s string) bool {
	if len(s) != Length {
		return false
	}
	for _, c := range s {
		if !strings.ContainsRune(alphabet, c) {
			return false
		}
	}
	return true
}

// secretEncoding is base32 without padding, lower-cased: a secret that survives
// a URL, a cookie and a copy and paste.
var secretEncoding = base32.StdEncoding.WithPadding(base32.NoPadding)

// Secret returns n random bytes in base32, for a session's secret or a token's.
// n is at least 16: a secret shorter than that is not one.
func Secret(n int) string {
	if n < 16 {
		panic("id: a secret is at least 16 bytes")
	}
	raw := make([]byte, n)
	if _, err := rand.Read(raw); err != nil {
		panic(fmt.Sprintf("id: reading random bytes: %v", err))
	}
	return strings.ToLower(secretEncoding.EncodeToString(raw))
}
