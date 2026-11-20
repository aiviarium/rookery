package docs

import (
	"context"
	"encoding/binary"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"

	"github.com/aiviarium/rookery/internal/data"
)

// fixedCaller is a Caller that always returns one principal.
type fixedCaller struct{ principal data.Principal }

// Principal is the caller.
func (c fixedCaller) Principal(context.Context) (data.Principal, error) { return c.principal, nil }

// websocketServer serves the document handler for one principal.
func websocketServer(t *testing.T, d *documents, principal data.Principal) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	mux.Handle(DocumentPath, NewHandler(d.Documents, fixedCaller{principal: principal}))
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	return server
}

// dial opens a document's WebSocket.
func dial(t *testing.T, server *httptest.Server, documentID string, from int64) *websocket.Conn {
	t.Helper()
	url := "ws" + strings.TrimPrefix(server.URL, "http") + DocumentPath + documentID
	if from > 0 {
		url += "?from=" + strconv.FormatInt(from, 10)
	}
	// The response's body is the connection itself, which the connection
	// closes.
	connection, _, err := websocket.Dial(t.Context(), url, nil) //nolint:bodyclose // the body is the connection
	if err != nil {
		t.Fatalf("dialing %s: %v", url, err)
	}
	t.Cleanup(func() { _ = connection.CloseNow() })
	return connection
}

// TestWebSocketSync checks two browsers syncing one document.
func TestWebSocketSync(t *testing.T) {
	t.Parallel()

	d := newDocuments(t)
	ctx := t.Context()
	document := d.created(t, "yjs")
	if _, err := d.Append(ctx, d.user, document.ID, []byte("first")); err != nil {
		t.Fatalf("appending: %v", err)
	}
	server := websocketServer(t, d, d.user)

	first := dial(t, server, document.ID, 0)
	frame := readFrame(t, first)
	if frame.kind != frameUpdate || frame.sequence != 1 || string(frame.data) != "first" {
		t.Fatalf("the first frame = %+v", frame)
	}

	second := dial(t, server, document.ID, 0)
	frame = readFrame(t, second)
	if frame.kind != frameUpdate || frame.sequence != 1 {
		t.Fatalf("the second client's first frame = %+v", frame)
	}

	// An update from the first browser reaches the second, with its sequence.
	write := append([]byte{frameUpdate}, []byte("second")...)
	if err := first.Write(ctx, websocket.MessageBinary, write); err != nil {
		t.Fatalf("writing an update: %v", err)
	}
	frame = readFrame(t, second)
	if frame.kind != frameUpdate || frame.sequence != 2 || string(frame.data) != "second" {
		t.Fatalf("the relayed frame = %+v", frame)
	}

	// An upgrade from a browser that already had the first update gets the
	// second alone.
	late := dial(t, server, document.ID, 1)
	frame = readFrame(t, late)
	if frame.kind != frameUpdate || frame.sequence != 2 {
		t.Fatalf("the late client's frame = %+v", frame)
	}
}

// TestWebSocketSnapshotAndAwareness checks the snapshot frame and that awareness
// is relayed and not stored.
func TestWebSocketSnapshotAndAwareness(t *testing.T) {
	t.Parallel()

	d := newDocuments(t)
	ctx := t.Context()
	document := d.created(t, "yjs")
	for i := 1; i <= 2; i++ {
		if _, err := d.Append(ctx, d.user, document.ID, []byte{byte(i)}); err != nil {
			t.Fatalf("appending: %v", err)
		}
	}
	if _, err := d.Snapshot(ctx, d.user, document.ID, 2, []byte("state"), false, ""); err != nil {
		t.Fatalf("writing a snapshot: %v", err)
	}
	server := websocketServer(t, d, d.user)

	first := dial(t, server, document.ID, 0)
	frame := readFrame(t, first)
	if frame.kind != frameSnapshot || frame.sequence != 2 || string(frame.data) != "state" {
		t.Fatalf("the snapshot frame = %+v", frame)
	}

	second := dial(t, server, document.ID, 0)
	readFrame(t, second) // the snapshot, again
	if err := first.Write(ctx, websocket.MessageBinary, append([]byte{frameAwareness}, "here"...)); err != nil {
		t.Fatalf("writing awareness: %v", err)
	}
	frame = readFrame(t, second)
	if frame.kind != frameAwareness || string(frame.data) != "here" {
		t.Fatalf("the awareness frame = %+v", frame)
	}

	var stored int
	if err := d.Documents.store.Pool().QueryRow(ctx,
		"SELECT count(*) FROM rookery.document_updates WHERE document_id = $1", document.ID).Scan(&stored); err != nil {
		t.Fatalf("counting the updates: %v", err)
	}
	if stored != 2 {
		t.Errorf("the log holds %d updates, want 2", stored)
	}
}

// TestWebSocketRefused checks that a stranger is closed out.
func TestWebSocketRefused(t *testing.T) {
	t.Parallel()

	d := newDocuments(t)
	document := d.created(t, "yjs")
	stranger := data.Principal{Kind: data.PrincipalUser, ID: "02USER"}
	server := websocketServer(t, d, stranger)

	url := "ws" + strings.TrimPrefix(server.URL, "http") + DocumentPath + document.ID
	connection, _, err := websocket.Dial(t.Context(), url, nil) //nolint:bodyclose // the body is the connection
	if err != nil {
		t.Fatalf("dialing: %v", err)
	}
	defer func() { _ = connection.CloseNow() }()

	read, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	_, _, err = connection.Read(read)
	if err == nil {
		t.Fatal("a refused connection stayed open")
	}
	// The library's error for a close the peer sent is wrapped differently
	// depending on the path it took, so the test checks the status by name.
	if status := websocket.CloseStatus(err); status != websocket.StatusPolicyViolation &&
		!strings.Contains(err.Error(), "StatusPolicyViolation") {
		t.Errorf("the close = %v, want a policy violation", err)
	}
}

// frame is a decoded server frame.
type frame struct {
	kind     byte
	sequence int64
	data     []byte
}

// readFrame reads one frame or fails the test.
func readFrame(t *testing.T, connection *websocket.Conn) frame {
	t.Helper()
	read, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	kind, data, err := connection.Read(read)
	if err != nil {
		t.Fatalf("reading a frame: %v", err)
	}
	if kind != websocket.MessageBinary || len(data) == 0 {
		t.Fatalf("a frame of kind %v and %d bytes", kind, len(data))
	}
	decoded := frame{kind: data[0]}
	switch data[0] {
	case frameUpdate, frameSnapshot:
		decoded.sequence = int64(binary.BigEndian.Uint64(data[1:9])) //nolint:gosec // the server writes a sequence
		decoded.data = data[9:]
	case frameResync:
		decoded.sequence = int64(binary.BigEndian.Uint64(data[1:9])) //nolint:gosec // the server writes a sequence
	case frameAwareness:
		decoded.data = data[1:]
	}
	return decoded
}
