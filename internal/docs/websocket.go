package docs

import (
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"net/http"
	"strconv"
	"strings"

	"github.com/coder/websocket"
	"golang.org/x/sync/errgroup"

	"github.com/aiviarium/rookery/internal/data"
)

// The frames a browser and the server send over /ws/documents/{id}. Each
// message is one binary WebSocket frame, and starts with a kind byte:
//
//	client → server
//	  0x01            an update: the bytes after it
//	  0x02            an awareness message: the bytes after it
//
//	server → client
//	  0x01 + 8 bytes  an update: its sequence, big-endian, then the bytes
//	  0x02            an awareness message: the bytes after it
//	  0x03 + 8 bytes  a snapshot: the last sequence it covers, then the state
//	  0x04 + 8 bytes  a resync: the sequence the following snapshot covers
//
// The sequence in the query, ?from=SEQUENCE, is the last update the client has,
// and zero asks for everything the server keeps. A browser's client half gets
// these frames from @rookery/client; the gRPC stream carries the same messages
// as protobuf.
const (
	frameUpdate    byte = 0x01
	frameAwareness byte = 0x02
	frameSnapshot  byte = 0x03
	frameResync    byte = 0x04
)

// DocumentPath is where the edge mounts the handler.
const DocumentPath = "/ws/documents/"

// maxFrameSize is the largest frame a client may send.
const maxFrameSize = 4 << 20

// Handler serves a browser syncing one live document at
// /ws/documents/{id}.
type Handler struct {
	documents *Documents
	caller    data.Caller
}

// NewHandler returns the WebSocket handler for live documents.
func NewHandler(documents *Documents, caller data.Caller) *Handler {
	return &Handler{documents: documents, caller: caller}
}

// ServeHTTP upgrades the request and syncs the document until either side goes
// away.
func (h *Handler) ServeHTTP(writer http.ResponseWriter, request *http.Request) {
	if request.Method != http.MethodGet {
		http.Error(writer, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	id := strings.TrimPrefix(request.URL.Path, DocumentPath)
	if id == "" || strings.Contains(id, "/") {
		http.Error(writer, "not found", http.StatusNotFound)
		return
	}

	ctx, cancel := context.WithCancel(request.Context())
	defer cancel()

	principal, err := h.principal(ctx)
	if err != nil {
		http.Error(writer, "unauthenticated", http.StatusUnauthorized)
		return
	}

	from := int64(0)
	if raw := request.URL.Query().Get("from"); raw != "" {
		parsed, err := strconv.ParseInt(raw, 10, 64)
		if err != nil || parsed < 0 {
			http.Error(writer, "from is not a sequence", http.StatusBadRequest)
			return
		}
		from = parsed
	}

	connection, err := websocket.Accept(writer, request, &websocket.AcceptOptions{
		// The edge serves the browser's own origin, and the cookie is
		// SameSite=Lax; the default same-origin check is the right one.
		InsecureSkipVerify: false,
	})
	if err != nil {
		return
	}
	connection.SetReadLimit(maxFrameSize)
	closed := false
	defer func() {
		if !closed {
			_ = connection.CloseNow()
		}
	}()

	sync, err := h.documents.Sync(ctx, principal, id, from)
	if err != nil {
		reason := "the document is not there"
		if errors.Is(err, ErrForbidden) {
			reason = "not allowed"
		}
		closed = closeConnection(connection, websocket.StatusPolicyViolation, reason)
		return
	}
	defer func() { _ = sync.Close() }()

	group, groupCtx := errgroup.WithContext(ctx)
	group.Go(func() error { return h.write(groupCtx, connection, sync) })
	group.Go(func() error { return h.read(groupCtx, connection, sync) })
	switch err := group.Wait(); {
	case errors.Is(err, context.Canceled), err == nil:
		closed = closeConnection(connection, websocket.StatusNormalClosure, "")
	default:
		closed = closeConnection(connection, websocket.StatusInternalError, "the sync ended")
	}
}

// closeConnection closes the connection with a reason, waiting for the close
// frame to go, and reports whether it did.
func closeConnection(connection *websocket.Conn, status websocket.StatusCode, reason string) bool {
	return connection.Close(status, reason) == nil
}

// write sends the document's messages to the browser.
func (h *Handler) write(ctx context.Context, connection *websocket.Conn, sync *Sync) error {
	for {
		select {
		case <-ctx.Done():
			return nil
		case message, ok := <-sync.Messages():
			if !ok {
				return nil
			}
			frame, err := encodeFrame(message)
			if err != nil {
				return err
			}
			if err := connection.Write(ctx, websocket.MessageBinary, frame); err != nil {
				return err
			}
		}
	}
}

// read takes the browser's frames to the document.
func (h *Handler) read(ctx context.Context, connection *websocket.Conn, sync *Sync) error {
	for {
		kind, data, err := connection.Read(ctx)
		if err != nil {
			return err
		}
		if kind != websocket.MessageBinary || len(data) == 0 {
			continue
		}

		message := SyncMessage{Data: data[1:]}
		switch data[0] {
		case frameUpdate:
			message.Kind = KindUpdate
		case frameAwareness:
			message.Kind = KindAwareness
		default:
			return fmt.Errorf("%w: frame %#x is not one a client sends", ErrInvalid, data[0])
		}
		if len(message.Data) == 0 {
			continue
		}
		if err := sync.Send(ctx, message); err != nil {
			return err
		}
	}
}

// encodeFrame is a message as the browser's frame.
func encodeFrame(message SyncMessage) ([]byte, error) {
	switch message.Kind {
	case KindUpdate:
		frame := make([]byte, 1+8+len(message.Data))
		frame[0] = frameUpdate
		binary.BigEndian.PutUint64(frame[1:9], uint64(message.Sequence)) //nolint:gosec // a sequence is never negative
		copy(frame[9:], message.Data)
		return frame, nil
	case KindAwareness:
		return append([]byte{frameAwareness}, message.Data...), nil
	case KindSnapshot:
		frame := make([]byte, 1+8+len(message.Data))
		frame[0] = frameSnapshot
		binary.BigEndian.PutUint64(frame[1:9], uint64(message.Sequence)) //nolint:gosec // a sequence is never negative
		copy(frame[9:], message.Data)
		return frame, nil
	case KindResync:
		frame := make([]byte, 1+8)
		frame[0] = frameResync
		binary.BigEndian.PutUint64(frame[1:9], uint64(message.Sequence)) //nolint:gosec // a sequence is never negative
		return frame, nil
	default:
		return nil, fmt.Errorf("%w: %q is not a message the server sends", ErrInvalid, message.Kind)
	}
}

// principal reads the caller from the request's context.
func (h *Handler) principal(ctx context.Context) (data.Principal, error) {
	if h.caller == nil {
		return data.Principal{}, fmt.Errorf("%w: the document handler has no caller", ErrForbidden)
	}
	return h.caller.Principal(ctx)
}
