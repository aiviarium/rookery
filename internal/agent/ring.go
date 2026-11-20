package agent

// ring keeps the last size bytes written to it, and counts every byte ever
// written, so a reader can say where it stopped.
//
// It is not safe for concurrent use; a session guards it with its own lock.
type ring struct {
	buf   []byte
	start int
	size  int
	total uint64
}

func newRing(size int) *ring {
	return &ring{buf: make([]byte, size)}
}

// write appends p, dropping the oldest bytes when the ring is full.
func (r *ring) write(p []byte) {
	r.total += uint64(len(p))

	if len(p) >= len(r.buf) {
		copy(r.buf, p[len(p)-len(r.buf):])
		r.start = 0
		r.size = len(r.buf)
		return
	}

	end := (r.start + r.size) % len(r.buf)
	copied := copy(r.buf[end:], p)
	copy(r.buf, p[copied:])
	r.size += len(p)
	if over := r.size - len(r.buf); over > 0 {
		r.start = (r.start + over) % len(r.buf)
		r.size = len(r.buf)
	}
}

// since returns what is kept from offset on, the offset it actually starts at,
// and whether the bytes the caller asked for from were overwritten already.
func (r *ring) since(offset uint64) (data []byte, from uint64, truncated bool) {
	oldest := r.total - uint64(r.size) //nolint:gosec // size is at most a uint64's worth.
	if offset < oldest {
		offset = oldest
		truncated = true
	}
	if offset > r.total {
		offset = r.total
	}

	skip := int(offset - oldest) //nolint:gosec // at most len(r.buf).
	data = make([]byte, r.size-skip)
	first := copy(data, r.buf[(r.start+skip)%len(r.buf):])
	copy(data[first:], r.buf[:])
	return data, offset, truncated
}
