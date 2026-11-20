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
	"errors"
	"fmt"
	"hash/fnv"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
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

// Config is what a document store is opened with.
type Config struct {
	// Store is the database the documents live in.
	Store *store.Store

	// Authorizer answers a principal's level on a document.
	Authorizer data.Authorizer
}

// Documents stores and relays live documents.
type Documents struct {
	store       *store.Store
	auth        data.Authorizer
	hub         *hub
	appendSlots [64]chan struct{}
}

// New returns a document store.
func New(config Config) *Documents {
	d := &Documents{
		store: config.Store,
		auth:  config.Authorizer,
		hub:   newHub(),
	}
	for i := range d.appendSlots {
		d.appendSlots[i] = make(chan struct{}, 1)
	}
	return d
}

// appendSlot serializes commits and publication for a document on the leader
// without holding a database connection while waiting. Stripes bound memory.
func (d *Documents) appendSlot(id string) chan struct{} {
	hash := fnv.New32a()
	_, _ = hash.Write([]byte(id))
	return d.appendSlots[hash.Sum32()%uint32(len(d.appendSlots))]
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
	if _, err := access.ParseResource(parent); err != nil {
		return Document{}, err
	}
	if err := d.require(ctx, principal, parent, writeLevel); err != nil {
		return Document{}, err
	}

	var document Document
	err := d.transaction(ctx, func(tx pgx.Tx) error {
		q := d.store.Queries().WithTx(tx)
		row, err := q.CreateDocument(ctx, store.CreateDocumentParams{
			ID: id.New(), Type: kind, Parent: parent, OwnerID: principal.String(),
		})
		if err != nil {
			return fmt.Errorf("docs: creating a document: %w", err)
		}
		document = documentOf(row)
		if err := q.UpsertResource(ctx, store.UpsertResourceParams{Uri: parent}); err != nil {
			return fmt.Errorf("docs: recording the document's parent: %w", err)
		}
		if err := q.UpsertResource(ctx, store.UpsertResourceParams{Uri: document.URI(), Parent: &parent}); err != nil {
			return fmt.Errorf("docs: recording the document resource: %w", err)
		}
		if principal.Kind == access.KindUser {
			_, err := q.UpsertGrant(ctx, store.UpsertGrantParams{
				Resource: document.URI(), Principal: principal.String(), Level: int16(access.Manage),
			})
			if err != nil {
				return fmt.Errorf("docs: granting the document creator: %w", err)
			}
		}
		return nil
	})
	if err != nil {
		return Document{}, err
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

// List returns the documents the caller may read, newest first. A document the
// caller may not read is left out rather than refused, so one person's
// documents do not hide the rest.
func (d *Documents) List(ctx context.Context, principal data.Principal, parent string, limit int) ([]Document, error) {
	if limit <= 0 {
		limit = 100
	}
	if limit > 500 {
		limit = 500
	}
	rows, err := d.store.Queries().ListDocuments(ctx, store.ListDocumentsParams{
		Parent:   nullable(parent),
		RowLimit: int32(limit),
	})
	if err != nil {
		return nil, fmt.Errorf("docs: listing documents: %w", err)
	}
	documents := make([]Document, 0, len(rows))
	for _, row := range rows {
		document := documentOf(row)
		if err := d.require(ctx, principal, document.URI(), readLevel); err != nil {
			if errors.Is(err, ErrForbidden) {
				continue
			}
			return nil, err
		}
		documents = append(documents, document)
	}
	return documents, nil
}

// nullable is a value sqlc's narg reads as SQL NULL when it is empty.
func nullable(value string) *string {
	if value == "" {
		return nil
	}
	return &value
}

// Delete marks a document deleted and drops it as an access resource. The caller
// needs manage on it.
func (d *Documents) Delete(ctx context.Context, principal data.Principal, id string) error {
	document, err := d.requireDocument(ctx, principal, id, manageLevel)
	if err != nil {
		return err
	}
	return d.transaction(ctx, func(tx pgx.Tx) error {
		q := d.store.Queries().WithTx(tx)
		deleted, err := q.DeleteDocument(ctx, id)
		if err != nil {
			return fmt.Errorf("docs: deleting the document: %w", err)
		}
		if deleted == 0 {
			return fmt.Errorf("%w: no document %s", ErrNotFound, id)
		}
		if _, err := q.DeleteResource(ctx, document.URI()); err != nil {
			return fmt.Errorf("docs: dropping the document resource: %w", err)
		}
		return nil
	})
}

func (d *Documents) load(ctx context.Context, id string) (Document, error) {
	row, err := d.store.Queries().GetDocument(ctx, id)
	if errors.Is(err, pgx.ErrNoRows) {
		return Document{}, fmt.Errorf("%w: no document %s", ErrNotFound, id)
	}
	if err != nil {
		return Document{}, fmt.Errorf("docs: reading document %s: %w", id, err)
	}
	return documentOf(row), nil
}

func documentOf(row store.RookeryDocuments) Document {
	return Document{
		ID: row.ID, Type: row.Type, Parent: row.Parent, Owner: row.OwnerID,
		CreatedAt: row.CreatedAt, UpdatedAt: row.UpdatedAt,
	}
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
