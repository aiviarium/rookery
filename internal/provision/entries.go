// Package provision is the core's side of provisioners: the entries that name a
// provisioner type and its engine, the resources those entries made, the
// cloud-init seed a machine starts with, and the running of a provisioner's
// executable for one call.
//
// A plugin declares provisioner types; an entry is an admin's instance of one,
// with CBOR arguments and either a node (a managed host whose engine it drives
// through the server) or a remote address. What an entry makes is a resource: the
// core records it, lists it and removes it, and knows one kind itself, machine,
// of which a host has at most one.
//
// The provisioner's executable is not kept running. The server starts it for a
// call, with ROOKERY_PLUGIN_SOCKET, ROOKERY_SOCKET and a token in its
// environment, calls ProvisionerService on the socket it names, and stops it
// afterwards; its stderr is a line of the job's log.
package provision

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/store"
)

// KindMachine is the one resource kind the core knows: a machine for a host, at
// most one per host.
const KindMachine = "machine"

// ErrNoType is a provisioner type no installed plugin declares. It is
// access.ErrNotFound, so the edge answers NotFound rather than Internal.
var ErrNoType = access.ErrNotFound

// Entry is a provisioner entry: a name, the type an installed plugin declares,
// the arguments, and where its engine is.
type Entry struct {
	// Name is the entry's name, and the resource provisioner://Name.
	Name string

	// Type is the provisioner type's name, "plugin/id".
	Type string

	// Plugin is the plugin whose package carries the type.
	Plugin string

	// Args are the entry's arguments, CBOR in Core Deterministic mode.
	Args []byte

	// Node is the managed host whose engine the entry drives. It, Remote and
	// Local are exclusive: an entry with none of them drives the server's own
	// engine.
	Node string

	// Remote is the address of an engine the entry reaches itself, with the
	// credentials in Args and its secrets.
	Remote string

	// CreatedAt and UpdatedAt are when the entry was added and last changed.
	CreatedAt time.Time
	UpdatedAt time.Time
}

// Local reports whether the entry drives the server's own engine.
func (e Entry) Local() bool { return e.Node == "" && e.Remote == "" }

// Validate checks an entry before it is written.
func (e Entry) Validate() error {
	switch {
	case strings.TrimSpace(e.Name) == "":
		return fmt.Errorf("%w: an entry needs a name", access.ErrInvalid)
	case len(e.Name) > 64:
		return fmt.Errorf("%w: a provisioner name is at most 64 characters", access.ErrInvalid)
	case strings.ContainsAny(e.Name, " \t\n"):
		return fmt.Errorf("%w: a provisioner name has no space", access.ErrInvalid)
	case strings.HasPrefix(e.Name, "/") || strings.HasSuffix(e.Name, "/"):
		return fmt.Errorf("%w: a provisioner name does not start or end with a slash", access.ErrInvalid)
	case !strings.Contains(e.Type, "/"):
		return fmt.Errorf("%w: %q is not a provisioner type, such as docker/container", access.ErrInvalid, e.Type)
	case e.Node != "" && e.Remote != "":
		return fmt.Errorf("%w: an entry is on a node or remote, not both", access.ErrInvalid)
	}
	return nil
}

// Entries reads and writes provisioner entries.
type Entries struct {
	store   *store.Store
	checker *access.Checker
}

// NewEntries returns the entries of a database.
func NewEntries(opened *store.Store, checker *access.Checker) *Entries {
	return &Entries{store: opened, checker: checker}
}

// ByName returns an entry, or access.ErrNotFound.
func (e *Entries) ByName(ctx context.Context, name string) (Entry, error) {
	row, err := e.store.Queries().GetProvisioner(ctx, name)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return Entry{}, fmt.Errorf("%w: no provisioner %s", access.ErrNotFound, name)
	case err != nil:
		return Entry{}, fmt.Errorf("provision: reading the provisioner %s: %w", name, err)
	}
	return entryOf(row), nil
}

// List is a page of entries the caller may view, by name.
func (e *Entries) List(ctx context.Context, principal access.Principal, cursor string, limit int) ([]Entry, error) {
	if !principal.Authenticated() {
		return nil, access.ErrUnauthenticated
	}

	var rows []store.RookeryProvisioners
	var err error
	if principal.IsAdmin() {
		rows, err = e.store.Queries().ListProvisioners(ctx, store.ListProvisionersParams{
			Cursor:   cursor,
			RowLimit: clampLimit(limit),
		})
	} else {
		principals, groupsErr := principalsOf(ctx, e.store, principal)
		if groupsErr != nil {
			return nil, groupsErr
		}
		rows, err = e.store.Queries().ListProvisionersVisibleTo(ctx, store.ListProvisionersVisibleToParams{
			Cursor:     cursor,
			Principals: principals,
			RowLimit:   clampLimit(limit),
		})
	}
	if err != nil {
		return nil, fmt.Errorf("provision: listing provisioners: %w", err)
	}
	out := make([]Entry, 0, len(rows))
	for _, row := range rows {
		out = append(out, entryOf(row))
	}
	return out, nil
}

// Add writes a new entry, and records provisioner://Name as a resource the
// caller may be granted.
func (e *Entries) Add(ctx context.Context, entry Entry) (Entry, error) {
	if err := entry.Validate(); err != nil {
		return Entry{}, err
	}
	tx, err := e.store.Pool().Begin(ctx)
	if err != nil {
		return Entry{}, fmt.Errorf("provision: beginning the add: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	queries := e.store.Queries().WithTx(tx)
	row, err := queries.CreateProvisioner(ctx, store.CreateProvisionerParams{
		Name:   entry.Name,
		Type:   entry.Type,
		Plugin: entry.Plugin,
		Args:   entry.Args,
		Node:   entry.Node,
		Remote: entry.Remote,
	})
	if err != nil {
		if isUniqueViolation(err) {
			return Entry{}, fmt.Errorf("%w: a provisioner called %s is there already", access.ErrConflict, entry.Name)
		}
		return Entry{}, fmt.Errorf("provision: adding the provisioner %s: %w", entry.Name, err)
	}
	if err := queries.UpsertResource(ctx, store.UpsertResourceParams{Uri: access.ProvisionerURI(entry.Name)}); err != nil {
		return Entry{}, fmt.Errorf("provision: recording the provisioner's resource: %w", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return Entry{}, fmt.Errorf("provision: committing the add: %w", err)
	}
	return entryOf(row), nil
}

// Update changes an entry's arguments and where its engine is. Both Node and
// Remote empty leaves the entry where it is.
func (e *Entries) Update(ctx context.Context, name string, args []byte, node, remote string) (Entry, error) {
	current, err := e.ByName(ctx, name)
	if err != nil {
		return Entry{}, err
	}
	if node == "" && remote == "" {
		node, remote = current.Node, current.Remote
	}
	if node != "" && remote != "" {
		return Entry{}, fmt.Errorf("%w: an entry is on a node or remote, not both", access.ErrInvalid)
	}
	row, err := e.store.Queries().UpdateProvisioner(ctx, store.UpdateProvisionerParams{
		Name:   name,
		Args:   args,
		Node:   node,
		Remote: remote,
	})
	if err != nil {
		return Entry{}, fmt.Errorf("provision: updating the provisioner %s: %w", name, err)
	}
	return entryOf(row), nil
}

// Remove removes an entry and its resource row, refusing while resources it made
// are still recorded.
func (e *Entries) Remove(ctx context.Context, name string) error {
	if _, err := e.ByName(ctx, name); err != nil {
		return err
	}
	made, err := e.store.Queries().CountProvisionedResources(ctx, name)
	if err != nil {
		return fmt.Errorf("provision: counting what %s made: %w", name, err)
	}
	if made > 0 {
		return fmt.Errorf("%w: %s still has %d resources; remove them first", access.ErrConflict, name, made)
	}

	tx, err := e.store.Pool().Begin(ctx)
	if err != nil {
		return fmt.Errorf("provision: beginning the remove: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	queries := e.store.Queries().WithTx(tx)
	if _, err := queries.DeleteProvisioner(ctx, name); err != nil {
		return fmt.Errorf("provision: removing the provisioner %s: %w", name, err)
	}
	if _, err := queries.DeleteResource(ctx, access.ProvisionerURI(name)); err != nil {
		return fmt.Errorf("provision: removing the provisioner's resource: %w", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return fmt.Errorf("provision: committing the remove: %w", err)
	}
	return nil
}

func entryOf(row store.RookeryProvisioners) Entry {
	return Entry{
		Name:      row.Name,
		Type:      row.Type,
		Plugin:    row.Plugin,
		Args:      row.Args,
		Node:      row.Node,
		Remote:    row.Remote,
		CreatedAt: row.CreatedAt,
		UpdatedAt: row.UpdatedAt,
	}
}

// principalsOf is the grant principals a caller matches.
func principalsOf(ctx context.Context, opened *store.Store, principal access.Principal) ([]string, error) {
	principals := []string{principal.String(), access.Everyone}
	if principal.Kind != access.KindUser {
		return principals, nil
	}
	groups, err := opened.Queries().ListGroupsForUser(ctx, principal.ID)
	if err != nil {
		return nil, fmt.Errorf("provision: reading the caller's groups: %w", err)
	}
	for _, group := range groups {
		principals = append(principals, access.GroupPrincipal(group.ID))
	}
	return principals, nil
}

// clampLimit bounds a page size.
func clampLimit(limit int) int32 {
	switch {
	case limit <= 0:
		return 100
	case limit > 500:
		return 500
	default:
		return int32(limit)
	}
}

// isUniqueViolation reports a Postgres unique constraint error.
func isUniqueViolation(err error) bool {
	var pgErr interface{ SQLState() string }
	return errors.As(err, &pgErr) && pgErr.SQLState() == "23505"
}
