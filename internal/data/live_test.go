package data

import (
	"context"
	"fmt"
	"sync"
	"testing"
	"time"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/filter"
)

// run starts the change feed and stops it when the test ends.
func (d *datastore) run(t *testing.T) {
	t.Helper()

	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan struct{})
	go func() {
		defer close(done)
		if err := d.Run(ctx); err != nil && ctx.Err() == nil {
			t.Errorf("running the feed: %v", err)
		}
	}()
	t.Cleanup(func() {
		cancel()
		select {
		case <-done:
		case <-time.After(10 * time.Second):
			t.Error("the feed did not stop")
		}
	})
}

// TestChanges checks the stream from a cursor: the database's changes in order,
// then the ones written while it follows.
func TestChanges(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	d.run(t)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	principal := d.writer(t)

	for i := range 3 {
		d.insertNote(t, principal, map[string]any{"title": fmt.Sprintf("note %d", i)})
	}

	changes := make(chan Change, 16)
	errors := make(chan error, 1)
	go func() {
		errors <- d.Changes(ctx, 0, []string{"example/notes"}, func(change Change) error {
			changes <- change
			return nil
		})
	}()

	first := receive(t, changes, errors)
	if first.Kind != ChangeInsert || first.Collection != "example/notes" || first.Record["title"] != "note 0" {
		t.Fatalf("the first change = %v", first)
	}
	second := receive(t, changes, errors)
	if second.Seq <= first.Seq {
		t.Fatalf("the changes are out of order: %d then %d", first.Seq, second.Seq)
	}
	receive(t, changes, errors)

	// A write while the stream follows arrives on it.
	written := d.insertNote(t, principal, map[string]any{"title": "later"})
	following := receive(t, changes, errors)
	if following.Record["id"] != written["id"] || following.Kind != ChangeInsert {
		t.Fatalf("the followed change = %v", following)
	}

	// From a cursor: only what comes after.
	cancel()
	select {
	case err := <-errors:
		if err != nil {
			t.Fatalf("the stream ended with %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("the stream did not end with its context")
	}
}

// TestLive checks a live query: the snapshot, then inserts, updates and
// removals as records enter and leave it.
func TestLive(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	d.run(t)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	principal := d.writer(t)

	parsed, err := filter.ParseText("done = true", d.mustCollection(t, "example/notes").FilterFields())
	if err != nil {
		t.Fatalf("parsing the filter: %v", err)
	}

	events := make(chan LiveEvent, 64)
	errors := make(chan error, 1)
	go func() {
		errors <- d.Live(ctx, principal, LiveOptions{Collection: "example/notes", Filter: parsed, Space: space}, func(event LiveEvent) error {
			events <- event
			return nil
		})
	}()

	// The snapshot: one record that matches.
	matching := d.insertNote(t, principal, map[string]any{"title": "matches", "done": true})
	snapshot := receive(t, events, errors)
	if snapshot.Kind != LiveInsert || snapshot.Record["id"] != matching["id"] {
		t.Fatalf("the snapshot = %v", snapshot)
	}

	// An insert that does not match is no event; changing it to match is one.
	otherRecord := d.insertNote(t, principal, map[string]any{"title": "other"})
	updated, err := d.Update(ctx, principal, "example/notes", otherRecord["id"].(string), map[string]any{"done": true}, 0)
	if err != nil {
		t.Fatalf("updating: %v", err)
	}
	inserted := receive(t, events, errors)
	if inserted.Kind != LiveInsert || inserted.Resource != "record://example/notes/"+otherRecord["id"].(string) {
		t.Fatalf("the insert = %v", inserted)
	}

	// A record in another readable space is outside this subscription.
	d.grant(principal, "space://02SPACE", rookeryv1.Level_LEVEL_VIEW)
	d.grant(stranger, "space://02SPACE", rookeryv1.Level_LEVEL_MANAGE)
	if _, err := d.Insert(ctx, stranger, "example/notes", map[string]any{
		"title": "elsewhere", "done": true, "space_id": "02SPACE",
	}); err != nil {
		t.Fatalf("inserting elsewhere: %v", err)
	}

	// An update that keeps matching is an update.
	if _, err := d.Update(ctx, principal, "example/notes", otherRecord["id"].(string),
		map[string]any{"title": "renamed"}, updated["rev"].(int64)); err != nil {
		t.Fatalf("updating again: %v", err)
	}
	updatedEvent := receive(t, events, errors)
	if updatedEvent.Kind != LiveUpdate || updatedEvent.Record["title"] != "renamed" {
		t.Fatalf("the update = %v", updatedEvent)
	}

	// Changing it out of the filter is a removal.
	if _, err := d.Update(ctx, principal, "example/notes", otherRecord["id"].(string), map[string]any{"done": false}, 0); err != nil {
		t.Fatalf("updating out of the filter: %v", err)
	}
	removal := receive(t, events, errors)
	if removal.Kind != LiveRemove || removal.Resource != "record://example/notes/"+otherRecord["id"].(string) {
		t.Fatalf("the removal = %v", removal)
	}

	// A delete is a removal.
	if err := d.Delete(ctx, principal, "example/notes", matching["id"].(string), 0); err != nil {
		t.Fatalf("deleting: %v", err)
	}
	deleted := receive(t, events, errors)
	if deleted.Kind != LiveRemove || deleted.Record != nil {
		t.Fatalf("the delete = %v", deleted)
	}

	cancel()
	select {
	case err := <-errors:
		if err != nil {
			t.Fatalf("the live query ended with %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("the live query did not end with its context")
	}
}

// TestLiveResync checks that a subscriber that falls behind is told so, and gets
// the current state again.
func TestLiveResync(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	d.run(t)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	principal := d.writer(t)

	events := make(chan LiveEvent, 4096)
	errors := make(chan error, 1)
	reached := make(chan struct{})
	blocked := make(chan struct{})
	var once sync.Once
	go func() {
		errors <- d.Live(ctx, principal, LiveOptions{Collection: "example/notes", Space: space}, func(event LiveEvent) error {
			events <- event
			// The first event is where the subscriber stops reading: the
			// changes after it fill the subscription's buffer.
			once.Do(func() {
				close(reached)
				<-blocked
			})
			return nil
		})
	}()

	d.insertNote(t, principal, map[string]any{"title": "first"})
	receive(t, events, errors)
	<-reached

	// Fill the subscriber's buffer while it is blocked, and wait until the feed
	// has handed every one of them out.
	for i := range subscriberBuffer + 64 {
		d.insertNote(t, principal, map[string]any{"title": fmt.Sprintf("note %d", i)})
	}
	target := d.maxSequence(t)
	d.waitForFeed(t, target)
	close(blocked)

	// A resync arrives, and inserts follow it.
	seen := false
	deadline := time.After(20 * time.Second)
	for !seen {
		select {
		case event := <-events:
			if event.Kind == LiveResync {
				seen = true
			}
		case err := <-errors:
			t.Fatalf("the live query ended before the resync: %v", err)
		case <-deadline:
			t.Fatal("no resync arrived")
		}
	}
	for {
		select {
		case event := <-events:
			if event.Kind == LiveInsert {
				return
			}
		case err := <-errors:
			t.Fatalf("the live query ended after the resync: %v", err)
		case <-deadline:
			t.Fatal("no insert arrived after the resync")
		}
	}
}

// maxSequence is the newest change the database holds.
func (d *datastore) maxSequence(t *testing.T) int64 {
	t.Helper()
	var seq int64
	if err := d.Store().Pool().QueryRow(t.Context(), "SELECT coalesce(max(seq), 0) FROM rookery.changes").Scan(&seq); err != nil {
		t.Fatalf("reading the feed's sequence: %v", err)
	}
	return seq
}

// waitForFeed waits until the feed has handed out a sequence.
func (d *datastore) waitForFeed(t *testing.T, sequence int64) {
	t.Helper()
	deadline := time.Now().Add(20 * time.Second)
	for d.feedHead() < sequence {
		if time.Now().After(deadline) {
			t.Fatalf("the feed is at %d, not %d", d.feedHead(), sequence)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

// receive reads an event, or fails the test with the error the stream ended
// with.
func receive[T any](t *testing.T, events <-chan T, errors <-chan error) T {
	t.Helper()
	select {
	case event := <-events:
		return event
	case err := <-errors:
		t.Fatalf("the stream ended: %v", err)
		var zero T
		return zero
	case <-time.After(20 * time.Second):
		var zero T
		t.Fatalf("no event arrived")
		return zero
	}
}
