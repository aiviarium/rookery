package docs

import (
	"context"
	"errors"
	"fmt"
	"testing"
	"time"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/data"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// documents is a document store with a space the tests work in.
type documents struct {
	*Documents
	auth  *access.Checker
	user  data.Principal
	space string
}

func newDocuments(t *testing.T) *documents {
	t.Helper()
	opened := storetest.New(t)
	auth := access.NewChecker(opened)
	user := data.Principal{Kind: data.PrincipalUser, ID: "01USER"}
	space := "space://01SPACE"
	if err := auth.GrantUser(t.Context(), user.ID, space, access.Use); err != nil {
		t.Fatal(err)
	}
	return &documents{Documents: New(Config{Store: opened, Authorizer: auth}), auth: auth, user: user, space: space}
}

// created makes a document and fails the test when it cannot.
func (d *documents) created(t *testing.T, kind string) Document {
	t.Helper()
	document, err := d.Create(t.Context(), d.user, kind, d.space)
	if err != nil {
		t.Fatalf("creating a document: %v", err)
	}
	return document
}

// TestCreateGetDelete checks a document's life and the access it asks for.
func TestCreateGetDelete(t *testing.T) {
	t.Parallel()

	d := newDocuments(t)
	ctx := t.Context()

	document := d.created(t, "yjs")
	if document.ID == "" || document.Type != "yjs" || document.Parent != d.space || document.Owner != d.user.String() {
		t.Fatalf("the document = %+v", document)
	}
	resource, err := d.store.Queries().GetResource(ctx, document.URI())
	if err != nil || resource.Parent == nil || *resource.Parent != d.space {
		t.Fatalf("the document resource = %+v, %v", resource, err)
	}

	read, err := d.Get(ctx, d.user, document.ID)
	if err != nil {
		t.Fatalf("getting the document: %v", err)
	}
	if read.ID != document.ID || read.Type != document.Type {
		t.Errorf("the document read back = %+v", read)
	}

	// A stranger may not create under the space, and may not see the document.
	stranger := data.Principal{Kind: data.PrincipalUser, ID: "02USER"}
	if _, err := d.Create(ctx, stranger, "yjs", d.space); !errors.Is(err, ErrForbidden) {
		t.Errorf("creating without a level = %v, want ErrForbidden", err)
	}
	if _, err := d.Get(ctx, stranger, document.ID); !errors.Is(err, ErrForbidden) {
		t.Errorf("getting without a level = %v, want ErrForbidden", err)
	}

	// The creator's manage is what the granter gave, and a deletion needs it.
	if err := d.Delete(ctx, d.user, document.ID); err != nil {
		t.Fatalf("deleting: %v", err)
	}
	if _, err := d.Get(ctx, d.user, document.ID); !errors.Is(err, ErrNotFound) {
		t.Errorf("getting a deleted document = %v, want ErrNotFound", err)
	}
	if _, err := d.Get(ctx, d.user, "missing"); !errors.Is(err, ErrNotFound) {
		t.Errorf("getting a missing document = %v, want ErrNotFound", err)
	}
	if err := d.Delete(ctx, d.user, document.ID); !errors.Is(err, ErrNotFound) {
		t.Errorf("deleting a deleted document = %v, want ErrNotFound", err)
	}
}

// TestListShowsWhatTheCallerMayRead checks that a list leaves out a document a
// stranger may not read, and keeps the ones the caller may.
func TestListShowsWhatTheCallerMayRead(t *testing.T) {
	t.Parallel()

	d := newDocuments(t)
	ctx := t.Context()
	first := d.created(t, "yjs")
	second := d.created(t, "markdown")

	documents, err := d.List(ctx, d.user, "", 0)
	if err != nil {
		t.Fatalf("listing: %v", err)
	}
	if len(documents) != 2 {
		t.Fatalf("the caller sees %d documents, want 2", len(documents))
	}

	stranger := data.Principal{Kind: data.PrincipalUser, ID: "02USER"}
	documents, err = d.List(ctx, stranger, "", 0)
	if err != nil {
		t.Fatalf("listing as a stranger: %v", err)
	}
	if len(documents) != 0 {
		t.Fatalf("a stranger sees %d documents, want none", len(documents))
	}

	documents, err = d.List(ctx, d.user, d.space, 1)
	if err != nil || len(documents) != 1 {
		t.Fatalf("listing one under the parent = %v, %v", documents, err)
	}
	if documents[0].ID != first.ID && documents[0].ID != second.ID {
		t.Fatalf("the listed document is %s, which is neither", documents[0].ID)
	}
}

// TestSnapshotsAndPrune checks the snapshot, the history and the cleanup that
// follows a snapshot.
func TestSnapshotsAndPrune(t *testing.T) {
	t.Parallel()

	d := newDocuments(t)
	ctx := t.Context()
	document := d.created(t, "yjs")

	for i := 1; i <= 3; i++ {
		sequence, err := d.Append(ctx, d.user, document.ID, []byte(fmt.Sprintf("update %d", i)))
		if err != nil {
			t.Fatalf("appending update %d: %v", i, err)
		}
		if sequence != int64(i) {
			t.Fatalf("update %d took the sequence %d", i, sequence)
		}
	}
	if _, err := d.Append(ctx, d.user, document.ID, nil); !errors.Is(err, ErrInvalid) {
		t.Errorf("an empty update = %v, want ErrInvalid", err)
	}

	// A snapshot covering the second update, then a version covering all three.
	if _, err := d.Snapshot(ctx, d.user, document.ID, 2, []byte("state 2"), false, ""); err != nil {
		t.Fatalf("writing a snapshot: %v", err)
	}
	if _, err := d.Snapshot(ctx, d.user, document.ID, 3, []byte("state 3"), true, "after the third"); err != nil {
		t.Fatalf("writing a version: %v", err)
	}
	if _, err := d.Snapshot(ctx, d.user, document.ID, 9, []byte("state 9"), false, ""); !errors.Is(err, ErrInvalid) {
		t.Errorf("a snapshot beyond the log = %v, want ErrInvalid", err)
	}

	history, err := d.History(ctx, d.user, document.ID)
	if err != nil {
		t.Fatalf("reading the history: %v", err)
	}
	if len(history) != 1 || history[0].Sequence != 3 || history[0].Label != "after the third" ||
		history[0].CreatedBy != d.user.String() {
		t.Fatalf("the history = %+v", history)
	}

	// A sync from nothing gets the latest snapshot, then the updates after it.
	sync, err := d.Sync(ctx, d.user, document.ID, 0)
	if err != nil {
		t.Fatalf("syncing: %v", err)
	}
	defer func() { _ = sync.Close() }()
	first := receiveMessage(t, sync.Messages())
	if first.Kind != KindSnapshot || first.Sequence != 3 || string(first.Data) != "state 3" {
		t.Fatalf("the first message = %+v", first)
	}

	// A sync from before the snapshot gets the snapshot and no update.
	sync2, err := d.Sync(ctx, d.user, document.ID, 2)
	if err != nil {
		t.Fatalf("syncing: %v", err)
	}
	defer func() { _ = sync2.Close() }()
	if message := receiveMessage(t, sync2.Messages()); message.Kind != KindSnapshot || message.Sequence != 3 {
		t.Fatalf("the first message from sequence 2 = %+v", message)
	}

	// A day after a snapshot was written, the updates it covers go.
	if _, err := d.Documents.store.Pool().Exec(ctx,
		"UPDATE rookery.document_snapshots SET created_at = now() - interval '2 days' WHERE document_id = $1 AND seq = 2",
		document.ID); err != nil {
		t.Fatalf("backdating the snapshot: %v", err)
	}
	pruned, err := d.Prune(ctx)
	if err != nil {
		t.Fatalf("pruning: %v", err)
	}
	if pruned != 2 {
		t.Errorf("pruned %d updates, want 2", pruned)
	}

	var remaining []int64
	rows, err := d.Documents.store.Pool().Query(ctx,
		"SELECT seq FROM rookery.document_updates WHERE document_id = $1 ORDER BY seq", document.ID)
	if err != nil {
		t.Fatalf("reading the updates: %v", err)
	}
	defer rows.Close()
	for rows.Next() {
		var sequence int64
		if err := rows.Scan(&sequence); err != nil {
			t.Fatalf("reading the updates: %v", err)
		}
		remaining = append(remaining, sequence)
	}
	if len(remaining) != 1 || remaining[0] != 3 {
		t.Errorf("the updates left are %v, want [3]", remaining)
	}
	if _, err := d.Documents.store.Pool().Exec(ctx,
		"UPDATE rookery.document_snapshots SET created_at = now() - interval '2 days' WHERE document_id = $1",
		document.ID); err != nil {
		t.Fatal(err)
	}
	if count, err := d.Prune(ctx); err != nil || count != 1 {
		t.Fatalf("pruning the last update = %d, %v", count, err)
	}
	if seq, err := d.Append(ctx, d.user, document.ID, []byte("after pruning")); err != nil || seq != 4 {
		t.Fatalf("appending after a full prune = %d, %v", seq, err)
	}
}

// TestSyncRelay checks that a participant's update reaches the others, and that
// awareness is relayed and not stored.
func TestSyncRelay(t *testing.T) {
	t.Parallel()

	d := newDocuments(t)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	document := d.created(t, "yjs")

	writer, err := d.Sync(ctx, d.user, document.ID, 0)
	if err != nil {
		t.Fatalf("syncing the writer: %v", err)
	}
	defer func() { _ = writer.Close() }()
	reader, err := d.Sync(ctx, d.user, document.ID, 0)
	if err != nil {
		t.Fatalf("syncing the reader: %v", err)
	}
	defer func() { _ = reader.Close() }()

	if err := writer.Send(ctx, SyncMessage{Kind: KindUpdate, Data: []byte("first")}); err != nil {
		t.Fatalf("sending an update: %v", err)
	}
	relayed := receiveMessage(t, reader.Messages())
	if relayed.Kind != KindUpdate || relayed.Sequence != 1 || string(relayed.Data) != "first" {
		t.Fatalf("the relayed update = %+v", relayed)
	}

	// The writer does not get its own update back.
	select {
	case message := <-writer.Messages():
		t.Fatalf("the writer received %+v", message)
	case <-time.After(100 * time.Millisecond):
	}

	// Awareness reaches the others and is not stored.
	if err := writer.Send(ctx, SyncMessage{Kind: KindAwareness, Data: []byte("cursor")}); err != nil {
		t.Fatalf("sending awareness: %v", err)
	}
	awareness := receiveMessage(t, reader.Messages())
	if awareness.Kind != KindAwareness || string(awareness.Data) != "cursor" {
		t.Fatalf("the awareness message = %+v", awareness)
	}
	var stored int
	if err := d.Documents.store.Pool().QueryRow(ctx,
		"SELECT count(*) FROM rookery.document_updates WHERE document_id = $1", document.ID).Scan(&stored); err != nil {
		t.Fatalf("counting the updates: %v", err)
	}
	if stored != 1 {
		t.Errorf("the log holds %d updates, want 1", stored)
	}

	// A participant with only `view` syncs but cannot send.
	viewer := data.Principal{Kind: data.PrincipalUser, ID: "03USER"}
	if err := d.auth.GrantUser(t.Context(), viewer.ID, document.URI(), access.View); err != nil {
		t.Fatal(err)
	}
	watcher, err := d.Sync(ctx, viewer, document.ID, 0)
	if err != nil {
		t.Fatalf("syncing a viewer: %v", err)
	}
	defer func() { _ = watcher.Close() }()
	if err := watcher.Send(ctx, SyncMessage{Kind: KindUpdate, Data: []byte("nope")}); !errors.Is(err, ErrForbidden) {
		t.Fatalf("a viewer sending an update = %v, want ErrForbidden", err)
	}

	// A stranger does not sync at all.
	stranger := data.Principal{Kind: data.PrincipalUser, ID: "04USER"}
	if _, err := d.Sync(ctx, stranger, document.ID, 0); !errors.Is(err, ErrForbidden) {
		t.Fatalf("a stranger syncing = %v, want ErrForbidden", err)
	}
}

// TestSyncOrder checks that a participant joining late gets the log in order.
func TestSyncOrder(t *testing.T) {
	t.Parallel()

	d := newDocuments(t)
	ctx := t.Context()
	document := d.created(t, "yjs")

	for i := 1; i <= 3; i++ {
		if _, err := d.Append(ctx, d.user, document.ID, []byte(fmt.Sprintf("update %d", i))); err != nil {
			t.Fatalf("appending: %v", err)
		}
	}

	sync, err := d.Sync(ctx, d.user, document.ID, 0)
	if err != nil {
		t.Fatalf("syncing: %v", err)
	}
	defer func() { _ = sync.Close() }()

	for i := 1; i <= 3; i++ {
		message := receiveMessage(t, sync.Messages())
		if message.Kind != KindUpdate || message.Sequence != int64(i) || string(message.Data) != fmt.Sprintf("update %d", i) {
			t.Fatalf("message %d = %+v", i, message)
		}
	}

	// A sync from the second update gets the third alone.
	late, err := d.Sync(ctx, d.user, document.ID, 2)
	if err != nil {
		t.Fatalf("syncing late: %v", err)
	}
	defer func() { _ = late.Close() }()
	message := receiveMessage(t, late.Messages())
	if message.Sequence != 3 {
		t.Fatalf("the first message from sequence 2 = %+v", message)
	}
}

// TestSyncResync checks that a participant that falls behind is told so and gets
// the state again.
func TestSyncResync(t *testing.T) {
	t.Parallel()

	d := newDocuments(t)
	ctx := t.Context()
	document := d.created(t, "yjs")

	slow, err := d.Sync(ctx, d.user, document.ID, 0)
	if err != nil {
		t.Fatalf("syncing: %v", err)
	}
	defer func() { _ = slow.Close() }()

	// More updates than the participant's outgoing buffer, while it reads none.
	for i := range outgoingBuffer + participantBuffer + 8 {
		if _, err := d.Append(ctx, d.user, document.ID, []byte(fmt.Sprintf("update %d", i))); err != nil {
			t.Fatalf("appending: %v", err)
		}
	}

	// The first messages are the buffered ones; a resync follows once the
	// buffer ran out.
	deadline := time.After(20 * time.Second)
	seen := false
	for !seen {
		select {
		case message := <-slow.Messages():
			if message.Kind == KindResync {
				seen = true
			}
		case <-deadline:
			t.Fatal("no resync arrived")
		}
	}
	// Then the log from the start, in order.
	next := int64(1)
	for next <= 3 {
		message := receiveMessage(t, slow.Messages())
		if message.Kind != KindUpdate || message.Sequence != next {
			t.Fatalf("after the resync came %+v, want update %d", message, next)
		}
		next++
	}
}

// receiveMessage reads a message or fails the test.
func receiveMessage(t *testing.T, messages <-chan SyncMessage) SyncMessage {
	t.Helper()
	select {
	case message := <-messages:
		return message
	case <-time.After(20 * time.Second):
		t.Fatalf("no message arrived")
		return SyncMessage{}
	}
}
