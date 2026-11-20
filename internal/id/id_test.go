package id

import (
	"strings"
	"testing"
)

func TestNewIsSortableAndDistinct(t *testing.T) {
	t.Parallel()

	seen := make(map[string]bool, 1000)
	// The first ten characters are the 48-bit timestamp and its two padding
	// bits: ids made in the same millisecond sort by their random bits, so only
	// the timestamp is non-decreasing.
	var lastTime string
	for range 1000 {
		got := New()
		if !Valid(got) {
			t.Fatalf("New returned %q, which is not a valid id", got)
		}
		if seen[got] {
			t.Fatalf("New returned %q twice", got)
		}
		seen[got] = true
		if stamp := got[:10]; stamp < lastTime {
			t.Fatalf("New returned %q after %q: the timestamp of an id sorts by its creation time", got, lastTime)
		} else {
			lastTime = stamp
		}
	}
}

func TestValid(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name string
		id   string
		want bool
	}{
		{name: "made", id: "01hzzzzzzzzzzzzzzzzzzzzzzz", want: false},
		{name: "ours", id: New(), want: true},
		{name: "short", id: "01H", want: false},
		{name: "long", id: New() + "0", want: false},
		{name: "lower case", id: strings.ToLower(New()), want: false},
		{name: "not base32", id: "01HZZZZZZZZZZZZZZZZZZZZZU!", want: false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			if got := Valid(tc.id); got != tc.want {
				t.Errorf("Valid(%q) = %v, want %v", tc.id, got, tc.want)
			}
		})
	}
}

func TestSecret(t *testing.T) {
	t.Parallel()

	first, second := Secret(32), Secret(32)
	if first == second {
		t.Fatal("Secret returned the same value twice")
	}
	if len(first) != 52 {
		t.Errorf("a 32-byte secret is %d characters, want 52", len(first))
	}
	if strings.ToUpper(first) != first && strings.ToLower(first) != first {
		t.Errorf("Secret mixes cases: %q", first)
	}

	defer func() {
		if recover() == nil {
			t.Error("Secret accepted fewer than 16 bytes")
		}
	}()
	Secret(8)
}
