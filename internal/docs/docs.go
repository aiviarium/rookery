// Package docs is live documents.
//
// A document is a CRDT document of some type, such as yjs, with a parent
// resource its access comes from. The core never reads an update: it keeps them
// in an append-only log, relays each one to the document's other participants,
// and stores compacted snapshots that cover a sequence. Awareness messages are
// relayed and not stored, because they are about who is where, not about the
// document.
//
// Every read and write checks the level on document://<id>; the access package
// resolves a grant on the document's parent to the document itself.
package docs

import (
	"context"
	"fmt"
	"strings"
	"time"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/data"
	"github.com/aiviarium/rookery/internal/id"
	"github.com/aiviarium/rookery/internal/store"
)

// The errors this package reports. They wrap the datastore's, so that the one
// error mapping that turns them into connect codes keeps working.
var (
	// ErrNotFound is a document that is not there, or that the caller may not
	// see.
	ErrNotFound = data.ErrNotFound

	// ErrForbidden is a caller without the level an operation needs.
	ErrForbidden = data.ErrForbidden

	// ErrInvalid is a document, a sequence or a snapshot that is not valid.
	ErrInvalid = data.ErrInvalid
)

// The levels documents ask for: view to read and sync, use to append an update
// or compact, and manage to mark a version or delete.
const (
	readLevel   = rookeryv1.Level_LEVEL_VIEW
	writeLevel  = rookeryv1.Level_LEVEL_USE
	manageLevel = rookeryv1.Level_LEVEL_MANAGE
)

// Resources records a document as an access resource with its parent, and drops
// it again. The access package implements it; it is optional until it is wired.
type Resources interface {
	// Add records the resource uri with its parent.
	Add(ctx context.Context, uri, parent string) error

	// Remove drops the resource.
	Remove(ctx context.Context, uri string) error
}

// Granter gives the creator of a document manage on it. The access package
// implements it; it is optional until it is wired.
type Granter interface {
	// GrantOwner gives a principal manage on a resource.
	GrantOwner(ctx context.Context, uri string, principal data.Principal) error
}

// Config is what a document store is opened with.
type Config struct {
	// Store is the database the documents live in.
	Store *store.Store

	// Authorizer answers a principal's level on a document.
	Authorizer data.Authorizer

	// Resources records documents as resources. Optional.
	Resources Resources

	// Granter gives creators manage on their documents. Optional.
	Granter Granter
}

// Documents stores and relays live documents.
type Documents struct {
	store     *store.Store
	auth      data.Authorizer
	resources Resources
	granter   Granter
	hub       *hub
}

// New returns a document store.
func New(config Config) *Documents {
	return &Documents{
		store:     config.Store,
		auth:      config.Authorizer,
		resources: config.Resources,
		granter:   config.Granter,
		hub:       newHub(),
	}
}

// Document is one live document.
type Document struct {
	// ID is the document's id, a ULID.
	ID string
	// Type is the CRDT a client opens it with, such as "yjs".
	Type string
	// Parent is the resource URI whose grants reach the document.
	Parent string
	// Owner is who made it, as access writes a principal.
	Owner string
	// CreatedAt and UpdatedAt are its timestamps.
	CreatedAt time.Time
	UpdatedAt time.Time
}

// URI is the document's resource URI.
func (d Document) URI() string { return "document://" + d.ID }

// Create makes a document under a parent resource. The caller needs `use` on the
// parent, and gets manage on the document.
func (d *Documents) Create(ctx context.Context, principal data.Principal, kind, parent string) (Document, error) {
	kind = strings.TrimSpace(kind)
	if kind == "" || len(kind) > 64 || strings.ContainsAny(kind, "\x00/") {
		return Document{}, fmt.Errorf("%w: %q is not a document type", ErrInvalid, kind)
	}
	if !strings.Contains(parent, "://") {
		return Document{}, fmt.Errorf("%w: %q is not a resource URI", ErrInvalid, parent)
	}
	if err := d.require(ctx, principal, parent, writeLevel); err != nil {
		return Document{}, err
	}

	id := id.New()
	document := Document{ID: id, Type: kind, Parent: parent, Owner: principal.String()}
	err := d.store.Pool().QueryRow(ctx, `
		INSERT INTO rookery.documents (id, type, parent, owner_id)
		VALUES ($1, $2, $3, $4)
		RETURNING created_at, updated_at`, id, kind, parent, document.Owner).
		Scan(&document.CreatedAt, &document.UpdatedAt)
	if err != nil {
		return Document{}, fmt.Errorf("docs: creating a document: %w", err)
	}

	// The document is its own resource, hanging under its parent, and its
	// creator manages it. If either fails, the document goes again: a document
	// nobody can reach is worse than no document.
	if d.resources != nil {
		if err := d.resources.Add(ctx, document.URI(), parent); err != nil {
			_ = d.deleteRow(ctx, id)
			return Document{}, fmt.Errorf("docs: recording the document as a resource: %w", err)
		}
	}
	if d.granter != nil {
		if err := d.granter.GrantOwner(ctx, document.URI(), principal); err != nil {
			if d.resources != nil {
				_ = d.resources.Remove(ctx, document.URI())
			}
			_ = d.deleteRow(ctx, id)
			return Document{}, fmt.Errorf("docs: granting the creator of the document: %w", err)
		}
	}
	return document, nil
}

// Get returns a document the caller may read.
func (d *Documents) Get(ctx context.Context, principal data.Principal, id string) (Document, error) {
	document, err := d.load(ctx, id)
	if err != nil {
		return Document{}, err
	}
	if err := d.require(ctx, principal, document.URI(), readLevel); err != nil {
		return Document{}, err
	}
	return document, nil
}

// Delete marks a document deleted and drops it as an access resource. The caller
// needs manage on it.
func (d *Documents) Delete(ctx context.Context, principal data.Principal, id string) error {
	document, err := d.load(ctx, id)
	if err != nil {
		return err
	}
	if err := d.require(ctx, principal, document.URI(), manageLevel); err != nil {
		return err
	}

	_, err = d.store.Pool().Exec(ctx,
		"UPDATE rookery.documents SET deleted_at = now(), updated_at = now() WHERE id = $1", id)
	if err != nil {
		return fmt.Errorf("docs: deleting the document: %w", err)
	}
	if d.resources != nil {
		if err := d.resources.Remove(ctx, document.URI()); err != nil {
			return fmt.Errorf("docs: dropping the document as a resource: %w", err)
		}
	}
	return nil
}

// load reads a document that is not deleted.
func (d *Documents) load(ctx context.Context, id string) (Document, error) {
	var document Document
	err := d.store.Pool().QueryRow(ctx, `
		SELECT id, type, parent, owner_id, created_at, updated_at
		FROM rookery.documents
		WHERE id = $1 AND deleted_at IS NULL`, id).
		Scan(&document.ID, &document.Type, &document.Parent, &document.Owner, &document.CreatedAt, &document.UpdatedAt)
	if err != nil {
		return Document{}, fmt.Errorf("%w: no document %s", ErrNotFound, id)
	}
	return document, nil
}

// deleteRow removes a document's row, for a create that could not finish.
func (d *Documents) deleteRow(ctx context.Context, id string) error {
	_, err := d.store.Pool().Exec(ctx, "DELETE FROM rookery.documents WHERE id = $1", id)
	return err
}

// require checks a principal's level on a resource.
func (d *Documents) require(ctx context.Context, principal data.Principal, resource string, need rookeryv1.Level) error {
	if d.auth == nil {
		return fmt.Errorf("%w: the document store has no authorizer", ErrForbidden)
	}
	level, err := d.auth.Level(ctx, principal, resource)
	if err != nil {
		return fmt.Errorf("docs: checking %s on %s: %w", principal, resource, err)
	}
	if !data.Allows(level, need) {
		return fmt.Errorf("%w: %s has level %d on %s and needs %d", ErrForbidden, principal, level, resource, need)
	}
	return nil
}

// requireDocument loads a document and checks a level on it.
func (d *Documents) requireDocument(ctx context.Context, principal data.Principal, id string, need rookeryv1.Level) (Document, error) {
	document, err := d.load(ctx, id)
	if err != nil {
		return Document{}, err
	}
	if err := d.require(ctx, principal, document.URI(), need); err != nil {
		return Document{}, err
	}
	return document, nil
}
