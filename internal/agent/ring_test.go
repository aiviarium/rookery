package agent

import (
	"strings"
	"testing"
)

func TestRing(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name       string
		size       int
		writes     []string
		since      uint64
		want       string
		wantFrom   uint64
		wantCutOff bool
	}{
		{
			name:     "everything kept",
			size:     16,
			writes:   []string{"hello", " world"},
			want:     "hello world",
			wantFrom: 0,
		},
		{
			name:     "from an offset",
			size:     16,
			writes:   []string{"hello", " world"},
			since:    6,
			want:     "world",
			wantFrom: 6,
		},
		{
			name:     "past the end",
			size:     16,
			writes:   []string{"hello"},
			since:    99,
			want:     "",
			wantFrom: 5,
		},
		{
			name:       "the start was overwritten",
			size:       8,
			writes:     []string{"hello", " world"},
			since:      2,
			want:       "lo world",
			wantFrom:   3,
			wantCutOff: true,
		},
		{
			name:       "one write longer than the ring",
			size:       4,
			writes:     []string{"abcdefgh"},
			want:       "efgh",
			wantFrom:   4,
			wantCutOff: true,
		},
		{
			name:       "wrapping in the middle",
			size:       8,
			writes:     []string{"abcdef", "ghij"},
			want:       "cdefghij",
			wantFrom:   2,
			wantCutOff: true,
		},
	}

	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()

			buffer := newRing(test.size)
			for _, write := range test.writes {
				buffer.write([]byte(write))
			}

			data, from, truncated := buffer.since(test.since)
			if string(data) != test.want {
				t.Errorf("since(%d) = %q, want %q", test.since, data, test.want)
			}
			if from != test.wantFrom {
				t.Errorf("since(%d) starts at %d, want %d", test.since, from, test.wantFrom)
			}
			if truncated != test.wantCutOff {
				t.Errorf("since(%d) truncated = %v, want %v", test.since, truncated, test.wantCutOff)
			}
		})
	}
}

func TestRingTotalCountsEveryByte(t *testing.T) {
	t.Parallel()

	buffer := newRing(4)
	buffer.write([]byte("abcdef"))
	if buffer.total != 6 {
		t.Errorf("total = %d, want 6", buffer.total)
	}
	if got := string(mustSince(t, buffer, buffer.total)); got != "" {
		t.Errorf("reading from the end gives %q, want nothing", got)
	}
}

// TestRingLongSession keeps the ring honest over many writes: what it reports as
// kept is the tail of what was written.
func TestRingLongSession(t *testing.T) {
	t.Parallel()

	buffer := newRing(64)
	var written strings.Builder
	for i := 0; i < 100; i++ {
		line := strings.Repeat(string(rune('a'+i%26)), 7)
		buffer.write([]byte(line))
		written.WriteString(line)
	}

	data, from, truncated := buffer.since(0)
	if !truncated {
		t.Error("a reader from the start is not told that its output was overwritten")
	}
	if want := written.String()[written.Len()-64:]; string(data) != want {
		t.Errorf("kept %q, want %q", data, want)
	}
	if from != uint64(written.Len()-64) { //nolint:gosec // the builder is longer than the ring.
		t.Errorf("kept from %d, want %d", from, written.Len()-64)
	}
}

func mustSince(t *testing.T, buffer *ring, offset uint64) []byte {
	t.Helper()
	data, _, _ := buffer.since(offset)
	return data
}
