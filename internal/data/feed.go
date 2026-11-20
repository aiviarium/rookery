package data

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"sync/atomic"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/cborx"
)

// The feed's bounds.
const (
	// recentChanges is how many changes the feed keeps for a subscriber that
	// subscribes a little late. A subscriber further behind than this re-reads
	// the database instead.
	recentChanges = 4096

	// changeBatch is how many changes one read of the feed takes.
	changeBatch = 1000

	// feedPoll is how often the feed looks for changes it was not notified
	// about. Notifications are transactional, so this is only a safety net.
	feedPoll = 2 * time.Second

	// subscriberBuffer is how many changes one subscriber may fall behind
	// before it is told to resync.
	subscriberBuffer = 256
)

// subscription is one live query's view of the feed.
type subscription struct {
	// changes is buffered; a subscriber that fills it is behind.
	changes chan Change
	after   int64

	// behind is set when a change was dropped, so that the subscriber resyncs.
	behind atomic.Bool
}

// Run follows the change feed until the context ends, fanning new changes out
// to the live queries and the Changes streams.
//
// The leader runs one loop. It owns its connection, because the notification a
// write sends arrives on the connection LISTEN is on, which a pooled connection
// cannot promise. A change whose notification is missed is still read: the loop
// looks for changes every feedPoll.
func (d *Data) Run(ctx context.Context) error {
	connection, err := d.pool().Acquire(ctx)
	if err != nil {
		return fmt.Errorf("data: acquiring the feed's connection: %w", err)
	}
	defer connection.Release()

	if _, err := connection.Exec(ctx, "LISTEN rookery_changes"); err != nil {
		return fmt.Errorf("data: listening for changes: %w", err)
	}
	if err := d.feedStart(ctx); err != nil {
		return err
	}

	for {
		wait, cancel := context.WithTimeout(ctx, feedPoll)
		_, err := connection.Conn().WaitForNotification(wait)
		cancel()
		if ctx.Err() != nil {
			return nil
		}
		if err != nil && !errors.Is(err, context.DeadlineExceeded) {
			return fmt.Errorf("data: waiting for changes: %w", err)
		}
		if err := d.feedLoad(ctx); err != nil {
			return err
		}
	}
}

// Changes hands over the change feed from a sequence: every change after it,
// oldest first, and then each new one as it is written. An empty collections
// list follows every collection.
func (d *Data) Changes(ctx context.Context, from int64, collections []string, fn func(Change) error) error {
	if err := d.feedStarted(ctx); err != nil {
		return nil
	}
	next := from
	for {
		// Everything the database already holds, in order.
		for {
			batch, err := d.readChanges(ctx, next)
			if err != nil {
				return err
			}
			for _, change := range batch {
				next = change.Seq
				if wanted(change, collections) {
					if err := fn(change); err != nil {
						return err
					}
				}
			}
			if len(batch) < changeBatch {
				break
			}
		}

		// Then the live feed, from where the database stopped. A subscription
		// that cannot replay what it missed reads the database again.
		subscription, replay, ok := d.subscribe(next)
		if !ok {
			continue
		}
		followed, err := d.follow(ctx, subscription, replay, func(change Change) error {
			next = change.Seq
			if wanted(change, collections) {
				return fn(change)
			}
			return nil
		})
		if followed || err != nil {
			return err
		}
	}
}

// follow hands a subscription's changes over until the subscriber falls behind,
// which reports false so that Changes reads the database again, or until the
// context ends.
func (d *Data) follow(ctx context.Context, subscription *subscription, replay []Change, fn func(Change) error) (bool, error) {
	defer d.unsubscribe(subscription)

	for _, change := range replay {
		if err := fn(change); err != nil {
			return true, err
		}
	}
	for {
		select {
		case <-ctx.Done():
			return true, nil
		case change, ok := <-subscription.changes:
			if !ok {
				return true, nil
			}
			if subscription.behind.Load() {
				return false, nil
			}
			if err := fn(change); err != nil {
				return true, err
			}
		}
	}
}

// wanted reports whether a change is one of the collections a subscriber asked
// for.
func wanted(change Change, collections []string) bool {
	return len(collections) == 0 || slices.Contains(collections, change.Collection)
}

// readChanges reads a batch of the feed from a sequence.
func (d *Data) readChanges(ctx context.Context, from int64) ([]Change, error) {
	rows, err := d.pool().Query(ctx, `SELECT seq, collection, resource, kind, rev, space_id, owner_id, principal, data, at
		FROM rookery.changes WHERE seq > $1 ORDER BY seq LIMIT $2`, from, changeBatch)
	if err != nil {
		return nil, fmt.Errorf("data: reading the change feed: %w", err)
	}
	defer rows.Close()

	changes := make([]Change, 0, changeBatch)
	for rows.Next() {
		change, err := scanChange(rows)
		if err != nil {
			return nil, err
		}
		changes = append(changes, change)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("data: reading the change feed: %w", err)
	}
	return changes, nil
}

// scanChange reads one change row.
func scanChange(rows pgx.Rows) (Change, error) {
	var (
		change  Change
		kind    string
		spaceID *string
		ownerID *string
		data    []byte
	)
	err := rows.Scan(&change.Seq, &change.Collection, &change.Resource, &kind, &change.Rev,
		&spaceID, &ownerID, &change.Principal, &data, &change.At)
	if err != nil {
		return Change{}, fmt.Errorf("data: reading a change: %w", err)
	}
	change.Kind = ChangeKind(kind)
	change.At = change.At.UTC()
	if spaceID != nil {
		change.SpaceID = *spaceID
	}
	if ownerID != nil {
		change.OwnerID = *ownerID
	}
	if len(data) > 0 {
		record := map[string]any{}
		if err := cborx.Unmarshal(data, &record); err != nil {
			return Change{}, fmt.Errorf("data: decoding the change %d: %w", change.Seq, err)
		}
		change.Record = record
	}
	return change, nil
}

// feedStart reads where the feed is, and keeps the latest changes, so that a
// subscriber that opens just before the loop starts is replayed rather than
// missing a write.
func (d *Data) feedStart(ctx context.Context) error {
	rows, err := d.pool().Query(ctx, `SELECT seq, collection, resource, kind, rev, space_id, owner_id, principal, data, at
		FROM rookery.changes ORDER BY seq DESC LIMIT `+fmt.Sprint(recentChanges))
	if err != nil {
		return fmt.Errorf("data: reading the change feed's sequence: %w", err)
	}
	defer rows.Close()

	recent := make([]Change, 0, 64)
	var last int64
	for rows.Next() {
		change, err := scanChange(rows)
		if err != nil {
			return err
		}
		if change.Seq > last {
			last = change.Seq
		}
		recent = append(recent, change)
	}
	if err := rows.Err(); err != nil {
		return fmt.Errorf("data: reading the change feed's sequence: %w", err)
	}
	// Newest first until here, oldest first for the replay.
	slices.Reverse(recent)

	d.feedMu.Lock()
	d.feedLast = last
	d.feedRecent = recent
	if len(recent) == recentChanges {
		d.feedDiscarded = recent[0].Seq - 1
	}
	d.feedMu.Unlock()
	d.feedReadyOnce.Do(func() { close(d.feedReady) })
	return nil
}

// feedStarted waits until the feed has read where it is. A live query that
// reads before that could miss a change the feed starts after.
func (d *Data) feedStarted(ctx context.Context) error {
	select {
	case <-d.feedReady:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

// feedHead is the sequence the feed handed out last.
func (d *Data) feedHead() int64 {
	d.feedMu.Lock()
	defer d.feedMu.Unlock()
	return d.feedLast
}

// feedLoad reads the changes after the last one handed out and fans them out.
func (d *Data) feedLoad(ctx context.Context) error {
	for {
		from := d.feedHead()
		changes, err := d.readChanges(ctx, from)
		if err != nil {
			return err
		}
		if len(changes) == 0 {
			return nil
		}
		d.feedPublish(changes)
		if len(changes) < changeBatch {
			return nil
		}
	}
}

// feedPublish keeps changes for replay and sends them to the live queries.
func (d *Data) feedPublish(changes []Change) {
	d.feedMu.Lock()
	defer d.feedMu.Unlock()

	for _, change := range changes {
		if change.Seq <= d.feedLast {
			continue
		}
		d.feedLast = change.Seq
		d.feedRecent = append(d.feedRecent, change)
		if len(d.feedRecent) > recentChanges {
			d.feedDiscarded = d.feedRecent[0].Seq
			d.feedRecent[0] = Change{}
			d.feedRecent = d.feedRecent[1:]
		}
		for subscription := range d.feedSubscribers {
			if change.Seq <= subscription.after {
				continue
			}
			select {
			case subscription.changes <- change:
			default:
				// The subscriber is behind: it resyncs rather than falling
				// further.
				subscription.behind.Store(true)
			}
		}
	}
}

// subscribe adds a live query, replaying the changes after from when the feed
// still holds them. It reports false when it holds none of them, and the caller
// reads the database instead.
func (d *Data) subscribe(from int64) (*subscription, []Change, bool) {
	d.feedMu.Lock()
	defer d.feedMu.Unlock()

	sub := &subscription{changes: make(chan Change, subscriberBuffer), after: from}

	var replay []Change
	if from < d.feedLast {
		if from < d.feedDiscarded {
			return nil, nil, false
		}
		for _, change := range d.feedRecent {
			if change.Seq > from {
				replay = append(replay, change)
			}
		}
	}

	if d.feedSubscribers == nil {
		d.feedSubscribers = make(map[*subscription]struct{})
	}
	d.feedSubscribers[sub] = struct{}{}
	return sub, replay, true
}

// unsubscribe removes a live query.
func (d *Data) unsubscribe(sub *subscription) {
	d.feedMu.Lock()
	defer d.feedMu.Unlock()
	delete(d.feedSubscribers, sub)
	close(sub.changes)
}
