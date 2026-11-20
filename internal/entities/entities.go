// Package entities is what search and resolution find.
//
// A record whose collection declares an entity block has a row in
// rookery.entities, which the datastore writes with the record: its URI, its
// kind, its title, the text search indexes, its access parent, and an embedding
// that only a plugin writes. This package reads those rows: it resolves an
// entity by URI, resolves several, and searches by full text, by vector
// similarity, or both.
package entities

import (
	"context"
	"errors"
	"fmt"
	"math"
	"slices"
	"strconv"
	"strings"

	"github.com/jackc/pgx/v5"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/data"
	"github.com/aiviarium/rookery/internal/store"
)

// The errors this package reports. They wrap the datastore's, so that the one
// error mapping that turns them into connect codes keeps working.
var (
	// ErrNotFound is an entity that is not there, or that the caller may not
	// see.
	ErrNotFound = data.ErrNotFound

	// ErrForbidden is a caller without the level an entity's access parent
	// needs.
	ErrForbidden = data.ErrForbidden

	// ErrInvalid is a search that names nothing to search for.
	ErrInvalid = data.ErrInvalid
)

// Config is what a registry is opened with.
type Config struct {
	// Data is the datastore whose declarations and database the entities come
	// from.
	Data *data.Data

	// Authorizer answers a principal's level on an entity's access parent.
	Authorizer data.Authorizer
}

// Registry resolves and searches the entities of the declared collections.
type Registry struct {
	data *data.Data
	auth data.Authorizer
}

// New returns a registry backed by the datastore's live declarations.
func New(config Config) *Registry {
	return &Registry{data: config.Data, auth: config.Authorizer}
}

// Kind finds the collection that declares an entity kind.
func (r *Registry) Kind(kind string) (string, bool) {
	for _, collection := range r.data.Collections("") {
		if collection.Entity != nil && collection.Entity.Kind == kind {
			return collection.Key(), true
		}
	}
	return "", false
}

// Kinds lists declared entity kinds.
func (r *Registry) Kinds() []string {
	kinds := make([]string, 0)
	for _, collection := range r.data.Collections("") {
		if collection.Entity != nil {
			kinds = append(kinds, collection.Entity.Kind)
		}
	}
	slices.Sort(kinds)
	return slices.Compact(kinds)
}

// Entity is one result of resolution or search.
type Entity struct {
	// URI is the entity's resource URI.
	URI string
	// Kind is what it is, such as "Note".
	Kind string
	// Title is what a client shows for it.
	Title string
	// Collection is the record's collection, "plugin/collection", when the
	// entity is a record and its collection is declared.
	Collection string
	// Access is the resource whose grants decide who may see it.
	Access string
}

// Resolve returns one entity, when the principal may read it.
func (r *Registry) Resolve(ctx context.Context, principal data.Principal, uri string) (Entity, error) {
	entity, err := r.load(ctx, uri)
	if err != nil {
		return Entity{}, err
	}
	if err := r.require(ctx, principal, entity); err != nil {
		return Entity{}, err
	}
	return entity, nil
}

// ResolveMany returns the entities the principal may read, skipping the ones it
// may not and the ones that are not there.
func (r *Registry) ResolveMany(ctx context.Context, principal data.Principal, uris []string) ([]Entity, error) {
	if len(uris) == 0 {
		return nil, nil
	}

	rows, err := r.data.Store().Queries().FindEntities(ctx, uris)
	if err != nil {
		return nil, fmt.Errorf("entities: resolving entities: %w", err)
	}

	visible := make(map[string]Entity, len(rows))
	for _, row := range rows {
		entity := r.entity(row.Uri, row.Kind, row.Title, row.Access)
		allowed, err := r.allowed(ctx, principal, entity)
		if err != nil {
			return nil, err
		}
		if allowed {
			visible[entity.URI] = entity
		}
	}
	found := make([]Entity, 0, len(visible))
	for _, uri := range uris {
		if entity, ok := visible[uri]; ok {
			found = append(found, entity)
			delete(visible, uri)
		}
	}
	return found, nil
}

// SearchResultLimit bounds a search, and defaultSearchLimit is what one page is
// when the caller does not say.
const (
	defaultSearchLimit = 20
	maxSearchLimit     = 100
)

// Search finds entities by full text, by vector similarity when an embedding is
// given, or both, and returns the ones the principal may read.
//
// The text query is Postgres's websearch syntax, so "rook -notes" is "rook but
// not notes". With an embedding the order is the text rank plus the cosine
// similarity, and with no text at all the order is the similarity alone. The
// core computes no embedding: a plugin that has one passes it here.
func (r *Registry) Search(ctx context.Context, principal data.Principal, query string, embedding []float32, limit int) ([]Entity, error) {
	if strings.TrimSpace(query) == "" && len(embedding) == 0 {
		return nil, fmt.Errorf("%w: a search needs a query or an embedding", ErrInvalid)
	}
	if limit <= 0 || limit > maxSearchLimit {
		limit = defaultSearchLimit
	}

	if len(embedding) > 16000 {
		return nil, fmt.Errorf("%w: an embedding can have at most 16000 dimensions", ErrInvalid)
	}
	for _, value := range embedding {
		if math.IsNaN(float64(value)) || math.IsInf(float64(value), 0) {
			return nil, fmt.Errorf("%w: an embedding must contain finite numbers", ErrInvalid)
		}
	}
	params := store.SearchEntitiesParams{TextQuery: query, RowLimit: 128}
	if len(embedding) != 0 {
		params.VectorQuery = vectorLiteral(embedding)
	}
	levels := make(map[struct {
		resource string
		level    rookeryv1.Level
	}]bool)
	results := make([]Entity, 0, limit)
	for len(results) < limit {
		rows, err := r.data.Store().Queries().SearchEntities(ctx, params)
		if err != nil {
			return nil, fmt.Errorf("entities: searching: %w", err)
		}
		for _, row := range rows {
			entity := r.entity(row.Uri, row.Kind, row.Title, row.Access)
			key := struct {
				resource string
				level    rookeryv1.Level
			}{entity.Access, r.readLevel(entity)}
			allowed, known := levels[key]
			if !known {
				allowed, err = r.allowed(ctx, principal, entity)
				if err != nil {
					return nil, err
				}
				levels[key] = allowed
			}
			if allowed {
				results = append(results, entity)
				if len(results) == limit {
					return results, nil
				}
			}
		}
		if len(rows) < int(params.RowLimit) {
			break
		}
		params.RowOffset += params.RowLimit
	}
	return results, nil
}

func (r *Registry) load(ctx context.Context, uri string) (Entity, error) {
	row, err := r.data.Store().Queries().FindEntity(ctx, uri)
	if errors.Is(err, pgx.ErrNoRows) {
		return Entity{}, fmt.Errorf("%w: no entity %s", ErrNotFound, uri)
	}
	if err != nil {
		return Entity{}, fmt.Errorf("entities: reading %s: %w", uri, err)
	}
	return r.entity(row.Uri, row.Kind, row.Title, row.Access), nil
}

func (r *Registry) entity(uri, kind, title, resource string) Entity {
	return Entity{URI: uri, Kind: kind, Title: title, Collection: r.collectionOf(uri), Access: resource}
}

// collectionOf is the collection a record entity is in, when it is declared.
func (r *Registry) collectionOf(uri string) string {
	key, _, err := data.ParseRecordURI(uri)
	if err != nil {
		return ""
	}
	if !r.data.Declared(key) {
		return ""
	}
	return key
}

// require checks a principal's level on an entity's access parent.
func (r *Registry) require(ctx context.Context, principal data.Principal, entity Entity) error {
	allowed, err := r.allowed(ctx, principal, entity)
	if err != nil {
		return err
	}
	if !allowed {
		return fmt.Errorf("%w: %s may not read %s", ErrForbidden, principal, entity.URI)
	}
	return nil
}

func (r *Registry) readLevel(entity Entity) rookeryv1.Level {
	if entity.Collection != "" {
		if collection, err := r.data.Collection(entity.Collection); err == nil {
			return collection.Access.Read
		}
	}
	return rookeryv1.Level_LEVEL_VIEW
}

func (r *Registry) allowed(ctx context.Context, principal data.Principal, entity Entity) (bool, error) {
	if r.auth == nil {
		return false, fmt.Errorf("%w: the entity registry has no authorizer", ErrForbidden)
	}
	level, err := r.auth.Level(ctx, principal, entity.Access)
	if err != nil {
		return false, fmt.Errorf("entities: checking %s on %s: %w", principal, entity.Access, err)
	}
	return data.Allows(level, r.readLevel(entity)), nil
}

// vectorLiteral is a vector as Postgres reads one.
func vectorLiteral(vector []float32) string {
	var builder strings.Builder
	builder.WriteByte('[')
	for i, number := range vector {
		if i > 0 {
			builder.WriteByte(',')
		}
		builder.WriteString(strconv.FormatFloat(float64(number), 'f', -1, 32))
	}
	builder.WriteByte(']')
	return builder.String()
}
