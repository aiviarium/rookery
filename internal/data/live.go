package data

import (
	"context"
	"fmt"

	"github.com/aiviarium/rookery/internal/filter"
)

// LiveKind is what a live query's event did.
type LiveKind string

const (
	// LiveInsert is a record that now matches and did not before.
	LiveInsert LiveKind = "insert"

	// LiveUpdate is a record that matched and still does.
	LiveUpdate LiveKind = "update"

	// LiveRemove is a record that no longer matches, was deleted, or may no
	// longer be read.
	LiveRemove LiveKind = "remove"

	// LiveResync tells a subscriber that it fell behind: the events it
	// received so far no longer describe the collection, and the inserts that
	// follow are the current state.
	LiveResync LiveKind = "resync"
)

// LiveEvent is one event of a live query.
type LiveEvent struct {
	// Kind is what happened.
	Kind LiveKind
	// Resource is the record's URI.
	Resource string
	// Record is the record after the change, and nil for a removal or a
	// resync.
	Record map[string]any
}

// LiveOptions is what a live query follows.
type LiveOptions struct {
	// Collection is "plugin/collection".
	Collection string
	// Filter selects the records. Nil matches every record.
	Filter *filter.Filter
	// Sorts order the snapshot, and are irrelevant to the changes after it.
	Sorts []Sort
	// Limit bounds the snapshot, as a Find's does.
	Limit int
	// Space is the caller's current space, which a space-scoped collection
	// needs.
	Space string
}

// The most changes a live query remembers matching records for.
const liveMaxTracked = 64 * 1024

// Live follows a query: the records that match now, then every change that
// makes a record start or stop matching, or that a caller may no longer read.
// It runs until the context ends or fn reports an error.
//
// A subscriber that falls behind is told so with LiveResync, and the inserts
// after it are the current state again. The level is checked for every change,
// and cached per scope resource, so a grant taken away mid-stream is noticed
// when the subscriber resyncs.
func (d *Data) Live(ctx context.Context, principal Principal, options LiveOptions, fn func(LiveEvent) error) error {
	collection, err := d.Collection(options.Collection)
	if err != nil {
		return err
	}
	if err := options.Filter.Validate(collection.FilterFields()); err != nil {
		return err
	}
	if err := collection.checkSorts(options.Sorts); err != nil {
		return err
	}

	if err := d.feedStarted(ctx); err != nil {
		return nil
	}

	// The subscription is taken before the snapshot, from the point the feed is
	// at: a write between the two is replayed, and a subscriber that falls
	// behind while the snapshot is sent resyncs.
	allowed := make(map[string]bool)
	for {
		from := d.feedHead()
		subscription, replay, ok := d.subscribe(from)
		if !ok {
			d.unsubscribe(subscription)
			if err := fn(LiveEvent{Kind: LiveResync}); err != nil {
				return err
			}
			continue
		}

		state := make(map[string]int64)
		if err := d.liveSnapshot(ctx, principal, collection, options, state, fn); err != nil {
			d.unsubscribe(subscription)
			return err
		}

		resync, err := d.followLive(ctx, principal, collection, options, subscription, replay, state, allowed, fn)
		if err != nil {
			return err
		}
		if !resync {
			return nil
		}
		if err := fn(LiveEvent{Kind: LiveResync}); err != nil {
			return err
		}
	}
}

// liveSnapshot sends the records that match now, remembering their revisions so
// that a replayed change to one is no news.
func (d *Data) liveSnapshot(ctx context.Context, principal Principal, collection *Collection, options LiveOptions,
	state map[string]int64, fn func(LiveEvent) error) error {

	page, err := d.Find(ctx, principal, collection.Key(), Query{
		Filter: options.Filter,
		Sorts:  options.Sorts,
		Limit:  options.Limit,
		Space:  options.Space,
	})
	if err != nil {
		return err
	}
	for _, record := range page.Records {
		id, _ := record["id"].(string)
		uri := collection.RecordURI(id)
		rev, _ := record["rev"].(int64)
		state[uri] = rev
		if err := fn(LiveEvent{Kind: LiveInsert, Resource: uri, Record: record}); err != nil {
			return err
		}
	}
	return nil
}

// followLive hands over the changes of one subscription. It reports true when
// the subscriber fell behind and has to resync.
func (d *Data) followLive(ctx context.Context, principal Principal, collection *Collection, options LiveOptions,
	subscription *subscription, replay []Change, state map[string]int64, allowed map[string]bool, fn func(LiveEvent) error) (bool, error) {

	defer d.unsubscribe(subscription)

	for _, change := range replay {
		if err := d.liveChange(ctx, principal, collection, options, change, state, allowed, fn); err != nil {
			return false, err
		}
	}
	for {
		select {
		case <-ctx.Done():
			return false, nil
		case change, ok := <-subscription.changes:
			if !ok {
				return false, nil
			}
			if subscription.behind.Load() {
				return true, nil
			}
			if err := d.liveChange(ctx, principal, collection, options, change, state, allowed, fn); err != nil {
				return false, err
			}
		}
	}
}

// liveChange turns one change into a live event, when it matters to the query.
func (d *Data) liveChange(ctx context.Context, principal Principal, collection *Collection, options LiveOptions,
	change Change, state map[string]int64, allowed map[string]bool, fn func(LiveEvent) error) error {

	if change.Collection != collection.Key() {
		return nil
	}
	uri := change.Resource
	seen, present := state[uri]

	readable, err := d.liveReadable(ctx, principal, collection, change, allowed)
	if err != nil {
		return err
	}
	if !readable {
		if present {
			delete(state, uri)
			return fn(LiveEvent{Kind: LiveRemove, Resource: uri})
		}
		return nil
	}

	// A change the subscriber has already been given, once the snapshot and
	// the replay overlap, is no news.
	latest := present && change.Rev <= seen

	switch change.Kind {
	case ChangeDelete:
		if !present {
			return nil
		}
		delete(state, uri)
		return fn(LiveEvent{Kind: LiveRemove, Resource: uri})
	case ChangeInsert, ChangeUpdate, ChangeUndelete:
		matches := true
		if options.Filter != nil && change.Record != nil {
			matching, err := options.Filter.Match(change.Record)
			if err != nil {
				return err
			}
			matches = matching
		}
		switch {
		case matches && latest:
			return nil
		case matches && present:
			state[uri] = change.Rev
			return fn(LiveEvent{Kind: LiveUpdate, Resource: uri, Record: recordMap(change.Record)})
		case matches:
			if len(state) >= liveMaxTracked {
				return fmt.Errorf("%w: the live query tracks more than %d records", ErrInvalid, liveMaxTracked)
			}
			state[uri] = change.Rev
			return fn(LiveEvent{Kind: LiveInsert, Resource: uri, Record: recordMap(change.Record)})
		case present:
			delete(state, uri)
			return fn(LiveEvent{Kind: LiveRemove, Resource: uri})
		default:
			return nil
		}
	default:
		return fmt.Errorf("%w: %q is not a change kind", ErrInvalid, change.Kind)
	}
}

// liveReadable reports whether the principal may read the record a change is
// about, remembering the answer per scope resource.
func (d *Data) liveReadable(ctx context.Context, principal Principal, collection *Collection, change Change, allowed map[string]bool) (bool, error) {
	resource := collection.resource(change.scopeID())
	if readable, known := allowed[resource]; known {
		return readable, nil
	}
	readable, err := d.allowed(ctx, principal, resource, cReadLevel)
	if err != nil {
		return false, err
	}
	if len(allowed) >= 4096 {
		// A stream that touches many scopes keeps the cache bounded; the
		// grants it forgets are asked again.
		clear(allowed)
	}
	allowed[resource] = readable
	return readable, nil
}

// scopeID is the scope id a change carries.
func (c Change) scopeID() string {
	if c.SpaceID != "" {
		return c.SpaceID
	}
	return c.OwnerID
}
