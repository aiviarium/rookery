package docs

import (
	"context"
	"fmt"
	"sync"
	"sync/atomic"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/data"
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
	from      int64
	// head is the log's sequence when the sync started: the state ends there,
	// and whatever comes after arrives through the hub.
	head int64

	participant *participant
	messages    chan SyncMessage
	resync      chan struct{}
	cancel      context.CancelFunc
	done        chan struct{}
	closeOnce   sync.Once
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
	head, err := d.head(ctx, id)
	if err != nil {
		return nil, err
	}

	ctx, cancel := context.WithCancel(ctx)
	joined := d.hub.join(id)
	sync := &Sync{
		documents:   d,
		document:    document,
		principal:   principal,
		from:        from,
		head:        head,
		participant: joined,
		messages:    make(chan SyncMessage, outgoingBuffer),
		resync:      joined.resync,
		cancel:      cancel,
		done:        make(chan struct{}),
	}

	// The initial state is handed out by the goroutine, so a slow reader does
	// not hold up the others.
	go sync.run(ctx)
	return sync, nil
}

// Messages are the messages to send the participant.
func (s *Sync) Messages() <-chan SyncMessage { return s.messages }

// Send relays a message the participant sent: an update is appended to the log
// and relayed to the others, and an awareness message is relayed only.
func (s *Sync) Send(ctx context.Context, message SyncMessage) error {
	switch message.Kind {
	case KindUpdate:
		sequence, err := s.documents.appendUpdate(ctx, s.principal, s.document.ID, message.Data, s.participant)
		if err != nil {
			return err
		}
		message.Sequence = sequence
		return nil
	case KindAwareness:
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
		s.documents.hub.leave(s.document.ID, s.participant)
	})
	return nil
}

// run hands the document's state and then the hub's messages to the outgoing
// channel, and resyncs the participant when it fell behind.
func (s *Sync) run(ctx context.Context) {
	defer close(s.done)
	defer close(s.messages)

	last, err := s.sendState(ctx, s.from, s.head)
	if err != nil {
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
			if last, err = s.sendResync(ctx); err != nil {
				return
			}
		case message := <-s.participant.messages:
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

// sendState sends what a participant at from needs, up to head: the latest
// snapshot when it is behind it, and the updates after that. It returns the last
// sequence it sent.
func (s *Sync) sendState(ctx context.Context, from, head int64) (int64, error) {
	sequence, data, err := s.documents.latestSnapshot(ctx, s.document.ID)
	if err != nil {
		return 0, err
	}
	if data != nil && from < sequence {
		if err := s.send(ctx, SyncMessage{Kind: KindSnapshot, Sequence: sequence, Data: data}); err != nil {
			return 0, err
		}
	}
	last := sequence
	if from > last {
		last = from
	}

	updates, err := s.documents.updatesAfter(ctx, s.document.ID, last, head)
	if err != nil {
		return 0, err
	}
	for _, update := range updates {
		if err := s.send(ctx, update); err != nil {
			return 0, err
		}
		last = update.Sequence
	}
	return last, nil
}

// sendResync tells the participant it fell behind and sends the state again. It
// returns the last sequence it sent.
func (s *Sync) sendResync(ctx context.Context) (int64, error) {
	head, err := s.documents.head(ctx, s.document.ID)
	if err != nil {
		return 0, err
	}
	sequence, data, err := s.documents.latestSnapshot(ctx, s.document.ID)
	if err != nil {
		return 0, err
	}
	if err := s.send(ctx, SyncMessage{Kind: KindResync, Sequence: sequence}); err != nil {
		return 0, err
	}
	if data != nil {
		if err := s.send(ctx, SyncMessage{Kind: KindSnapshot, Sequence: sequence, Data: data}); err != nil {
			return 0, err
		}
	}
	return s.sendState(ctx, sequence, head)
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
	document, err := d.requireDocument(ctx, principal, id, writeLevel)
	if err != nil {
		return 0, err
	}

	var sequence int64
	err = d.transaction(ctx, func(tx pgx.Tx) error {
		// One append at a time per document, so that two participants cannot
		// take the same sequence.
		if _, err := tx.Exec(ctx, "SELECT pg_advisory_xact_lock(hashtext($1))", "document:"+id); err != nil {
			return fmt.Errorf("docs: locking the document: %w", err)
		}
		if err := tx.QueryRow(ctx,
			"SELECT coalesce(max(seq), 0) + 1 FROM rookery.document_updates WHERE document_id = $1", id).
			Scan(&sequence); err != nil {
			return fmt.Errorf("docs: reading the document's sequence: %w", err)
		}
		_, err := tx.Exec(ctx,
			"INSERT INTO rookery.document_updates (document_id, seq, data) VALUES ($1, $2, $3)",
			id, sequence, update)
		if err != nil {
			return fmt.Errorf("docs: appending an update: %w", err)
		}
		_, err = tx.Exec(ctx, "UPDATE rookery.documents SET updated_at = now() WHERE id = $1", id)
		if err != nil {
			return fmt.Errorf("docs: touching the document: %w", err)
		}
		return nil
	})
	if err != nil {
		return 0, err
	}

	d.hub.publish(document.ID, SyncMessage{Kind: KindUpdate, Sequence: sequence, Data: update}, except)
	return sequence, nil
}

// head is a document's newest sequence.
func (d *Documents) head(ctx context.Context, id string) (int64, error) {
	var head int64
	err := d.store.Pool().QueryRow(ctx,
		"SELECT coalesce(max(seq), 0) FROM rookery.document_updates WHERE document_id = $1", id).Scan(&head)
	if err != nil {
		return 0, fmt.Errorf("docs: reading the document's sequence: %w", err)
	}
	return head, nil
}

// updatesAfter reads a document's updates after a sequence and up to another,
// oldest first.
func (d *Documents) updatesAfter(ctx context.Context, id string, after, until int64) ([]SyncMessage, error) {
	rows, err := d.store.Pool().Query(ctx,
		"SELECT seq, data FROM rookery.document_updates WHERE document_id = $1 AND seq > $2 AND seq <= $3 ORDER BY seq",
		id, after, until)
	if err != nil {
		return nil, fmt.Errorf("docs: reading the document's updates: %w", err)
	}
	defer rows.Close()

	updates := make([]SyncMessage, 0, 16)
	for rows.Next() {
		var (
			sequence int64
			data     []byte
		)
		if err := rows.Scan(&sequence, &data); err != nil {
			return nil, fmt.Errorf("docs: reading the document's updates: %w", err)
		}
		updates = append(updates, SyncMessage{Kind: KindUpdate, Sequence: sequence, Data: data})
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("docs: reading the document's updates: %w", err)
	}
	return updates, nil
}

// Prune deletes the updates a snapshot covers, a day after the snapshot was
// written. It is what keeps a busy document's log from growing without end; the
// server runs it periodically.
func (d *Documents) Prune(ctx context.Context) (int64, error) {
	tag, err := d.store.Pool().Exec(ctx, `
		DELETE FROM rookery.document_updates u
		WHERE EXISTS (
			SELECT 1 FROM rookery.document_snapshots s
			WHERE s.document_id = u.document_id
			  AND s.seq >= u.seq
			  AND s.created_at < now() - interval '1 day'
		)`)
	if err != nil {
		return 0, fmt.Errorf("docs: pruning snapshotted updates: %w", err)
	}
	return tag.RowsAffected(), nil
}
