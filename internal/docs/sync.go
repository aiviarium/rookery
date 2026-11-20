package docs

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"sync/atomic"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/data"
	"github.com/aiviarium/rookery/internal/store"
)

// SyncKind is what a sync message is.
type SyncKind string

const (
	// KindUpdate is a CRDT update: stored in the log, and relayed.
	KindUpdate SyncKind = "update"

	// KindAwareness is an awareness message: relayed, never stored.
	KindAwareness SyncKind = "awareness"

	// KindSnapshot is a compacted state the server sends first, with the
	// sequence it covers.
	KindSnapshot SyncKind = "snapshot"

	// KindResync tells a participant that it fell behind: a snapshot and the
	// updates after it follow.
	KindResync SyncKind = "resync"
)

// SyncMessage is one message a participant sends or receives.
type SyncMessage struct {
	// Kind is what it is.
	Kind SyncKind
	// Sequence is an update's place in the log, which the server assigns, and
	// the last sequence a snapshot or a resync covers.
	Sequence int64
	// Data is the CRDT update or the awareness message, opaque to the core.
	Data []byte
}

// The buffer of a participant's incoming messages, and of the messages its sync
// hands out. A participant that fills the first resyncs rather than stalls the
// others.
const (
	participantBuffer = 1024
	outgoingBuffer    = 256
)

// Sync is one participant's view of a document: the messages to send it, and the
// way to relay what it sends.
type Sync struct {
	documents *Documents
	document  Document
	principal data.Principal

	participant *participant
	messages    chan SyncMessage
	resync      chan struct{}
	cancel      context.CancelFunc
	done        chan struct{}
	closeOnce   sync.Once
	err         error // written before messages and done close
}

// hub relays a document's updates and awareness to its participants, in memory
// on the leader that holds the document's writes.
type hub struct {
	mu     sync.Mutex
	joined map[string]map[*participant]struct{}
}

// participant is one sync's place in the hub.
type participant struct {
	messages chan SyncMessage
	// behind is set when a message was dropped, and wakes the sync to resync.
	behind atomic.Bool
	resync chan struct{}
}

// newHub returns an empty hub.
func newHub() *hub {
	return &hub{joined: make(map[string]map[*participant]struct{})}
}

// join adds a participant to a document.
func (h *hub) join(id string) *participant {
	h.mu.Lock()
	defer h.mu.Unlock()

	joined := &participant{
		messages: make(chan SyncMessage, participantBuffer),
		resync:   make(chan struct{}, 1),
	}
	if h.joined[id] == nil {
		h.joined[id] = make(map[*participant]struct{})
	}
	h.joined[id][joined] = struct{}{}
	return joined
}

// leave removes a participant from a document.
func (h *hub) leave(id string, left *participant) {
	h.mu.Lock()
	defer h.mu.Unlock()

	delete(h.joined[id], left)
	if len(h.joined[id]) == 0 {
		delete(h.joined, id)
	}
}

// publish sends a message to a document's participants, other than except. A
// participant whose buffer is full is told to resync instead.
func (h *hub) publish(id string, message SyncMessage, except *participant) {
	h.mu.Lock()
	defer h.mu.Unlock()

	for participant := range h.joined[id] {
		if participant == except {
			continue
		}
		select {
		case participant.messages <- message:
		default:
			if !participant.behind.Swap(true) {
				select {
				case participant.resync <- struct{}{}:
				default:
				}
			}
		}
	}
}

// Sync subscribes a participant to a document, sending the latest snapshot and
// the updates after it, then every update and awareness message the others send.
// It runs until the context ends or Close is called.
//
// from is the sequence the participant already has, and zero asks for
// everything. The caller needs `view` on the document to sync, and `use` to send
// an update.
func (d *Documents) Sync(ctx context.Context, principal data.Principal, id string, from int64) (*Sync, error) {
	document, err := d.requireDocument(ctx, principal, id, readLevel)
	if err != nil {
		return nil, err
	}
	if from < 0 {
		return nil, fmt.Errorf("%w: a sequence is not negative", ErrInvalid)
	}

	joined := d.hub.join(id)
	state, err := d.readState(ctx, id, from)
	if err != nil {
		d.hub.leave(id, joined)
		return nil, err
	}
	ctx, cancel := context.WithCancel(ctx)
	sync := &Sync{
		documents:   d,
		document:    document,
		principal:   principal,
		participant: joined,
		messages:    make(chan SyncMessage, outgoingBuffer),
		resync:      joined.resync,
		cancel:      cancel,
		done:        make(chan struct{}),
	}

	// The initial state is handed out by the goroutine, so a slow reader does
	// not hold up the others.
	go sync.run(ctx, state, from)
	return sync, nil
}

// Messages are the messages to send the participant.
func (s *Sync) Messages() <-chan SyncMessage { return s.messages }

// Send relays a message the participant sent: an update is appended to the log
// and relayed to the others, and an awareness message is relayed only.
func (s *Sync) Send(ctx context.Context, message SyncMessage) error {
	switch message.Kind {
	case KindUpdate:
		_, err := s.documents.appendUpdate(ctx, s.principal, s.document.ID, message.Data, s.participant)
		return err
	case KindAwareness:
		if err := s.documents.require(ctx, s.principal, s.document.URI(), readLevel); err != nil {
			return err
		}
		message.Sequence = 0
		s.documents.hub.publish(s.document.ID, message, s.participant)
		return nil
	default:
		return fmt.Errorf("%w: %q is not a message a participant sends", ErrInvalid, message.Kind)
	}
}

// Close ends the sync and waits for what it started.
func (s *Sync) Close() error {
	s.closeOnce.Do(func() {
		s.cancel()
		<-s.done
	})
	return nil
}

// run hands the document's state and then the hub's messages to the outgoing
// channel, and resyncs the participant when it fell behind.
func (s *Sync) run(ctx context.Context, state documentState, from int64) {
	defer close(s.done)
	defer close(s.messages)
	defer s.documents.hub.leave(s.document.ID, s.participant)

	last, err := s.sendState(ctx, state, from, false)
	if err != nil {
		s.err = err
		return
	}
	for {
		select {
		case <-ctx.Done():
			return
		case <-s.resync:
			s.participant.behind.Store(false)
			// The messages buffered while the participant was behind are older
			// than the state it is about to get.
			s.drain()
			state, err := s.documents.readState(ctx, s.document.ID, 0)
			if err == nil {
				last, err = s.sendState(ctx, state, 0, true)
			}
			if err != nil {
				s.err = err
				return
			}
		case message := <-s.participant.messages:
			if _, err := s.documents.requireDocument(ctx, s.principal, s.document.ID, readLevel); err != nil {
				s.err = err
				return
			}
			if message.Kind == KindUpdate {
				// A message the state already covered is no news.
				if message.Sequence <= last {
					continue
				}
				last = message.Sequence
			}
			select {
			case s.messages <- message:
			case <-ctx.Done():
				return
			}
		}
	}
}

// Err reports why the stream stopped, after Messages has closed.
func (s *Sync) Err() error { return s.err }

// sendState takes both the snapshot and the updates from the same database
// view. Pruning cannot remove an update between those reads.
func (s *Sync) sendState(ctx context.Context, state documentState, from int64, resync bool) (int64, error) {
	snapshot := state.snapshot
	if resync || from > state.head {
		if err := s.send(ctx, SyncMessage{Kind: KindResync, Sequence: snapshot.Seq}); err != nil {
			return 0, err
		}
		from = 0
	}
	if snapshot.Data != nil && (from == 0 || from < snapshot.Seq) {
		if err := s.send(ctx, SyncMessage{Kind: KindSnapshot, Sequence: snapshot.Seq, Data: snapshot.Data}); err != nil {
			return 0, err
		}
	}
	for _, update := range state.updates {
		if err := s.send(ctx, SyncMessage{Kind: KindUpdate, Sequence: update.Seq, Data: update.Data}); err != nil {
			return 0, err
		}
	}
	return state.head, nil
}

type documentState struct {
	head     int64
	snapshot store.LatestDocumentSnapshotRow
	updates  []store.ListDocumentUpdatesRow
}

func (d *Documents) readState(ctx context.Context, id string, from int64) (documentState, error) {
	tx, err := d.store.Pool().BeginTx(ctx, pgx.TxOptions{IsoLevel: pgx.RepeatableRead, AccessMode: pgx.ReadOnly})
	if err != nil {
		return documentState{}, fmt.Errorf("docs: opening document state: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()
	q := d.store.Queries().WithTx(tx)
	document, err := q.GetDocument(ctx, id)
	if err != nil {
		return documentState{}, fmt.Errorf("docs: reading document state: %w", err)
	}
	snapshot, err := q.LatestDocumentSnapshot(ctx, id)
	if err != nil && !errors.Is(err, pgx.ErrNoRows) {
		return documentState{}, fmt.Errorf("docs: reading document snapshot: %w", err)
	}
	if from > document.Head {
		from = 0
	}
	updates, err := q.ListDocumentUpdates(ctx, store.ListDocumentUpdatesParams{
		DocumentID: id, After: max(from, snapshot.Seq), Through: document.Head,
	})
	if err != nil {
		return documentState{}, fmt.Errorf("docs: reading document updates: %w", err)
	}
	return documentState{head: document.Head, snapshot: snapshot, updates: updates}, nil
}

// send puts a message on the outgoing channel.
func (s *Sync) send(ctx context.Context, message SyncMessage) error {
	select {
	case s.messages <- message:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

// drain empties the participant's buffer.
func (s *Sync) drain() {
	for {
		select {
		case <-s.participant.messages:
		default:
			return
		}
	}
}

// Append adds an update to a document's log and relays it to the other
// participants, returning its sequence. The caller needs `use` on the document.
func (d *Documents) Append(ctx context.Context, principal data.Principal, id string, update []byte) (int64, error) {
	return d.appendUpdate(ctx, principal, id, update, nil)
}

// appendUpdate is Append for a sync, whose sender does not get its own update
// back.
func (d *Documents) appendUpdate(ctx context.Context, principal data.Principal, id string, update []byte, except *participant) (int64, error) {
	if len(update) == 0 {
		return 0, fmt.Errorf("%w: an update has bytes", ErrInvalid)
	}
	if _, err := d.requireDocument(ctx, principal, id, writeLevel); err != nil {
		return 0, err
	}
	slot := d.appendSlot(id)
	select {
	case slot <- struct{}{}:
		defer func() { <-slot }()
	case <-ctx.Done():
		return 0, ctx.Err()
	}
	var sequence int64
	err := d.transaction(ctx, func(tx pgx.Tx) error {
		q := d.store.Queries().WithTx(tx)
		var err error
		sequence, err = q.AdvanceDocument(ctx, id)
		if errors.Is(err, pgx.ErrNoRows) {
			return fmt.Errorf("%w: no document %s", ErrNotFound, id)
		}
		if err != nil {
			return fmt.Errorf("docs: advancing document sequence: %w", err)
		}
		if err := q.AppendDocumentUpdate(ctx, store.AppendDocumentUpdateParams{
			DocumentID: id, Seq: sequence, Data: update,
		}); err != nil {
			return fmt.Errorf("docs: appending document update: %w", err)
		}
		return nil
	})
	if err != nil {
		return 0, err
	}
	d.hub.publish(id, SyncMessage{Kind: KindUpdate, Sequence: sequence, Data: update}, except)
	return sequence, nil
}

// Prune deletes updates covered by a snapshot at least a day old.
func (d *Documents) Prune(ctx context.Context) (int64, error) {
	count, err := d.store.Queries().PruneDocumentUpdates(ctx)
	if err != nil {
		return 0, fmt.Errorf("docs: pruning snapshotted updates: %w", err)
	}
	return count, nil
}
