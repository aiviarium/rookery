package data

import (
	"context"
	"fmt"

	"github.com/jackc/pgx/v5"

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
// using the current grant on its scope.
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

	query := Query{Filter: options.Filter, Sorts: options.Sorts, Limit: options.Limit, Space: options.Space}
	for {
		page, from, err := d.liveSnapshot(ctx, principal, collection, query)
		if err != nil {
			return err
		}
		subscription, replay, ok := d.subscribe(from)
		if ok {
			resync, err := d.followSnapshot(ctx, principal, collection, options, page, subscription, replay, fn)
			if err != nil || !resync {
				return err
			}
		}
		if err := fn(LiveEvent{Kind: LiveResync}); err != nil {
			return err
		}
	}
}

// Read the snapshot and its cursor from one consistent database view.
func (d *Data) liveSnapshot(ctx context.Context, principal Principal, collection *Collection, query Query) (Page, int64, error) {
	where, args, err := d.scopeClause(ctx, principal, collection, query.Space)
	if err != nil {
		return Page{}, 0, err
	}
	feedHead := d.feedHead()
	tx, err := d.pool().BeginTx(ctx, pgx.TxOptions{IsoLevel: pgx.RepeatableRead, AccessMode: pgx.ReadOnly})
	if err != nil {
		return Page{}, 0, fmt.Errorf("data: opening live snapshot: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()
	var seq int64
	if err := tx.QueryRow(ctx, "SELECT coalesce(max(seq), 0) FROM rookery.changes").Scan(&seq); err != nil {
		return Page{}, 0, fmt.Errorf("data: reading live snapshot cursor: %w", err)
	}
	page, err := d.find(ctx, tx, collection, query, where, args)
	return page, max(seq, feedHead), err
}

func (d *Data) followSnapshot(ctx context.Context, principal Principal, collection *Collection, options LiveOptions,
	page Page, subscription *subscription, replay []Change, fn func(LiveEvent) error) (bool, error) {
	defer d.unsubscribe(subscription)
	state := make(map[string]int64, len(page.Records))
	for _, record := range page.Records {
		id, _ := record["id"].(string)
		uri := collection.RecordURI(id)
		rev, _ := record["rev"].(int64)
		state[uri] = rev
		if err := fn(LiveEvent{Kind: LiveInsert, Resource: uri, Record: record}); err != nil {
			return false, err
		}
	}
	return d.followLive(ctx, principal, collection, options, subscription, replay, state, fn)
}

// followLive hands over the changes of one subscription. It reports true when
// the subscriber fell behind and has to resync.
func (d *Data) followLive(ctx context.Context, principal Principal, collection *Collection, options LiveOptions,
	subscription *subscription, replay []Change, state map[string]int64, fn func(LiveEvent) error) (bool, error) {

	for _, change := range replay {
		if err := d.liveChange(ctx, principal, collection, options, change, state, fn); err != nil {
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
			if err := d.liveChange(ctx, principal, collection, options, change, state, fn); err != nil {
				return false, err
			}
		}
	}
}

// liveChange turns one change into a live event, when it matters to the query.
func (d *Data) liveChange(ctx context.Context, principal Principal, collection *Collection, options LiveOptions,
	change Change, state map[string]int64, fn func(LiveEvent) error) error {

	if change.Collection != collection.Key() {
		return nil
	}
	uri := change.Resource
	seen, present := state[uri]

	if collection.Scope == ScopeSpace && change.SpaceID != options.Space ||
		collection.Scope == ScopeUser && change.OwnerID != principal.ID {
		return nil
	}
	readable, err := d.allowed(ctx, principal, collection.resource(change.scopeID()), collection.Access.Read)
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

// scopeID is the scope id a change carries.
func (c Change) scopeID() string {
	if c.SpaceID != "" {
		return c.SpaceID
	}
	return c.OwnerID
}
