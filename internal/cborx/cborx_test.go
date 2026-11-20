package cborx_test

import (
	"bytes"
	"encoding/hex"
	"math"
	"reflect"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"

	"github.com/aiviarium/rookery/internal/cborx"
)

type record struct {
	ID     string            `cbor:"id"`
	Title  string            `cbor:"title"`
	Done   bool              `cbor:"done"`
	Count  int64             `cbor:"count"`
	Ratio  float64           `cbor:"ratio"`
	Tags   []string          `cbor:"tags"`
	Labels map[string]string `cbor:"labels"`
	About  []byte            `cbor:"about"`
}

func TestRoundTrip(t *testing.T) {
	t.Parallel()

	at := time.Date(2026, 11, 20, 9, 30, 15, 123456789, time.UTC)
	cases := []struct {
		name  string
		value any
		into  any
	}{
		{name: "struct", value: record{
			ID:     "01JC0M0M0M0M0M0M0M0M0M0M0M",
			Title:  "A note",
			Done:   true,
			Count:  -3,
			Ratio:  0.25,
			Tags:   []string{"a", "b"},
			Labels: map[string]string{"site": "lab", "role": "desk"},
			About:  []byte{0, 1, 2},
		}, into: &record{}},
		{name: "map", value: map[string]any{"n": int64(1), "s": "x", "l": []any{true, nil}}, into: &map[string]any{}},
		{name: "slice", value: []int64{1, 2, 3}, into: &[]int64{}},
		{name: "time", value: at, into: &time.Time{}},
		{name: "empty", value: map[string]any{}, into: &map[string]any{}},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			encoded, err := cborx.Marshal(tc.value)
			if err != nil {
				t.Fatalf("Marshal: %v", err)
			}
			if err := cborx.Unmarshal(encoded, tc.into); err != nil {
				t.Fatalf("Unmarshal: %v", err)
			}

			switch want := tc.value.(type) {
			case time.Time:
				if got := *tc.into.(*time.Time); !got.Equal(want) {
					t.Errorf("time: got %v, want %v", got, want)
				}
			default:
				got := reflect.Indirect(reflect.ValueOf(tc.into)).Interface()
				if diff := cmp.Diff(tc.value, got); diff != "" {
					t.Errorf("(-want +got):\n%s", diff)
				}
			}
		})
	}
}

// TestDeterministic is the property everything else relies on: one value, one
// encoding, whatever order its parts were built in.
func TestDeterministic(t *testing.T) {
	t.Parallel()

	first := map[string]any{"b": int64(2), "a": int64(1), "c": map[string]any{"y": "y", "x": "x"}}
	second := map[string]any{"c": map[string]any{"x": "x", "y": "y"}, "a": int64(1), "b": int64(2)}

	one, err := cborx.Marshal(first)
	if err != nil {
		t.Fatalf("Marshal: %v", err)
	}
	two, err := cborx.Marshal(second)
	if err != nil {
		t.Fatalf("Marshal: %v", err)
	}
	if !bytes.Equal(one, two) {
		t.Errorf("the same value encoded two ways:\n%s\n%s", hex.EncodeToString(one), hex.EncodeToString(two))
	}

	// Map keys sort bytewise, so "a" precedes "b" precedes "c", and each key
	// keeps its value next to it.
	if diff := cmp.Diff("a36161016162026163a26178617861796179", hex.EncodeToString(one)); diff != "" {
		t.Errorf("encoding (-want +got):\n%s", diff)
	}
}

func TestShortestFloat(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name  string
		value float64
		want  string // CBOR: 0xf9 is a half, 0xfb a double.
	}{
		{name: "half", value: 1.5, want: "f93e00"},
		{name: "double", value: 1.1, want: "fb3ff199999999999a"},
		{name: "half again", value: 3, want: "f94200"},
		{name: "pi", value: math.Pi, want: "fb400921fb54442d18"},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			encoded, err := cborx.Marshal(tc.value)
			if err != nil {
				t.Fatalf("Marshal: %v", err)
			}
			if got := hex.EncodeToString(encoded); got != tc.want {
				t.Errorf("got %s, want %s", got, tc.want)
			}
		})
	}
}

// TestRefused covers encodings that are legal CBOR but not the one encoding this
// package accepts.
func TestRefused(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name string
		data string
	}{
		// {"a": 1, "a": 1} with a duplicate key.
		{name: "duplicate map key", data: "a2616101616101"},
		// An indefinite-length array of one item: 0x9f 0x01 0xff.
		{name: "indefinite length", data: "9f01ff"},
		// An indefinite-length map: 0xbf 0x61 0x61 0x01 0xff.
		{name: "indefinite map", data: "bf616101ff"},
		// 0x62 followed by two bytes that are not UTF-8.
		{name: "invalid utf-8", data: "62c328"},
		// Truncated: says three bytes but carries one.
		{name: "truncated", data: "4301"},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			data, err := hex.DecodeString(tc.data)
			if err != nil {
				t.Fatalf("decoding the fixture: %v", err)
			}
			var into any
			if err := cborx.Unmarshal(data, &into); err == nil {
				t.Fatalf("Unmarshal accepted %s", tc.data)
			}
		})
	}
}

func TestNestingLimit(t *testing.T) {
	t.Parallel()

	// Depth+1 nested arrays, built innermost first.
	var value any = []any{}
	for range cborx.Depth + 1 {
		value = []any{value}
	}
	encoded, err := cborx.Marshal(value)
	if err != nil {
		t.Fatalf("Marshal: %v", err)
	}
	var into any
	if err := cborx.Unmarshal(encoded, &into); err == nil {
		t.Fatal("Unmarshal accepted a value nested deeper than the limit")
	}
}

func TestExtraFieldsIgnored(t *testing.T) {
	t.Parallel()

	type note struct {
		Title string `cbor:"title"`
	}
	encoded, err := cborx.Marshal(map[string]any{"title": "a", "later": true})
	if err != nil {
		t.Fatalf("Marshal: %v", err)
	}
	var got note
	if err := cborx.Unmarshal(encoded, &got); err != nil {
		t.Fatalf("Unmarshal: %v", err)
	}
	if got.Title != "a" {
		t.Errorf("title: got %q, want %q", got.Title, "a")
	}
}

func TestEncodeError(t *testing.T) {
	t.Parallel()

	if _, err := cborx.Marshal(func() {}); err == nil {
		t.Fatal("Marshal accepted a func")
	}
}

func FuzzUnmarshal(f *testing.F) {
	f.Add([]byte{0x01})
	f.Add([]byte{0xa1, 0x61, 0x61, 0x01})
	f.Add([]byte{0xf9, 0x3e, 0x00})
	f.Add([]byte{0x9f, 0x01, 0xff})

	f.Fuzz(func(t *testing.T, data []byte) {
		var into any
		if err := cborx.Unmarshal(data, &into); err != nil {
			return
		}
		// What decodes must encode, and encode to the same bytes again.
		first, err := cborx.Marshal(into)
		if err != nil {
			t.Fatalf("Marshal of a decoded value: %v", err)
		}
		var again any
		if err := cborx.Unmarshal(first, &again); err != nil {
			t.Fatalf("Unmarshal of an encoded value: %v", err)
		}
		second, err := cborx.Marshal(again)
		if err != nil {
			t.Fatalf("Marshal of a decoded value: %v", err)
		}
		if !bytes.Equal(first, second) {
			t.Errorf("encoding is not stable: %x, then %x", first, second)
		}
	})
}
