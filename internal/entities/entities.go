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
	"fmt"
	"sort"
	"strconv"
	"strings"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/data"
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
	data  *data.Data
	auth  data.Authorizer
	kinds map[string]string
}

// New returns a registry of the collections declared now.
func New(config Config) *Registry {
	registry := &Registry{
		data:  config.Data,
		auth:  config.Authorizer,
		kinds: make(map[string]string),
	}
	for _, collection := range config.Data.Collections("") {
		if collection.Entity == nil {
			continue
		}
		registry.kinds[collection.Entity.Kind] = collection.Key()
	}
	return registry
}

// Kind names the collection an entity kind belongs to.
func (r *Registry) Kind(kind string) (collection string, ok bool) {
	collection, ok = r.kinds[kind]
	return collection, ok
}

// Kinds are the declared entity kinds, sorted.
func (r *Registry) Kinds() []string {
	kinds := make([]string, 0, len(r.kinds))
	for kind := range r.kinds {
		kinds = append(kinds, kind)
	}
	sort.Strings(kinds)
	return kinds
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
	if err := r.require(ctx, principal, entity.Access); err != nil {
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

	rows, err := r.data.Store().Pool().Query(ctx, `
		SELECT uri, kind, title, access FROM rookery.entities WHERE uri = ANY($1)`, uris)
	if err != nil {
		return nil, fmt.Errorf("entities: resolving %d entities: %w", len(uris), err)
	}
	defer rows.Close()

	found := make([]Entity, 0, len(uris))
	for rows.Next() {
		var entity Entity
		if err := rows.Scan(&entity.URI, &entity.Kind, &entity.Title, &entity.Access); err != nil {
			return nil, fmt.Errorf("entities: resolving entities: %w", err)
		}
		allowed, err := r.allowed(ctx, principal, entity.Access)
		if err != nil {
			return nil, err
		}
		if !allowed {
			continue
		}
		entity.Collection = r.collectionOf(entity.URI)
		found = append(found, entity)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("entities: resolving entities: %w", err)
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

	args := []any{}
	where := ""
	if strings.TrimSpace(query) != "" {
		args = append(args, query)
		where = " WHERE search @@ websearch_to_tsquery('simple', $1)"
	}

	// The embedding is passed as pgvector's text form, so that the pool needs no
	// codec for the type.
	order := ""
	switch {
	case len(embedding) > 0 && where != "":
		args = append(args, vectorLiteral(embedding))
		order = " ORDER BY (ts_rank(search, websearch_to_tsquery('simple', $1)) + " +
			"coalesce(1 - (embedding <=> $2::vector), 0)) DESC, updated_at DESC"
	case len(embedding) > 0:
		args = append(args, vectorLiteral(embedding))
		order = " ORDER BY coalesce(1 - (embedding <=> $1::vector), 0) DESC, updated_at DESC"
	default:
		order = " ORDER BY ts_rank(search, websearch_to_tsquery('simple', $1)) DESC, updated_at DESC"
	}

	// More rows than asked for, because access may hide some.
	args = append(args, limit*4)
	statement := "SELECT uri, kind, title, access FROM rookery.entities" + where + order +
		fmt.Sprintf(" LIMIT $%d", len(args))

	rows, err := r.data.Store().Pool().Query(ctx, statement, args...)
	if err != nil {
		return nil, fmt.Errorf("entities: searching: %w", err)
	}
	defer rows.Close()

	levels := make(map[string]bool)
	results := make([]Entity, 0, limit)
	for rows.Next() && len(results) < limit {
		var entity Entity
		if err := rows.Scan(&entity.URI, &entity.Kind, &entity.Title, &entity.Access); err != nil {
			return nil, fmt.Errorf("entities: searching: %w", err)
		}
		allowed, known := levels[entity.Access]
		if !known {
			allowed, err = r.allowed(ctx, principal, entity.Access)
			if err != nil {
				return nil, err
			}
			levels[entity.Access] = allowed
		}
		if !allowed {
			continue
		}
		entity.Collection = r.collectionOf(entity.URI)
		results = append(results, entity)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("entities: searching: %w", err)
	}
	return results, nil
}

// load reads one entity's row.
func (r *Registry) load(ctx context.Context, uri string) (Entity, error) {
	var entity Entity
	err := r.data.Store().Pool().QueryRow(ctx,
		`SELECT uri, kind, title, access FROM rookery.entities WHERE uri = $1`, uri).
		Scan(&entity.URI, &entity.Kind, &entity.Title, &entity.Access)
	if err != nil {
		return Entity{}, fmt.Errorf("%w: no entity %s", ErrNotFound, uri)
	}
	entity.Collection = r.collectionOf(uri)
	return entity, nil
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
func (r *Registry) require(ctx context.Context, principal data.Principal, access string) error {
	allowed, err := r.allowed(ctx, principal, access)
	if err != nil {
		return err
	}
	if !allowed {
		return fmt.Errorf("%w: %s may not read %s", ErrForbidden, principal, access)
	}
	return nil
}

// allowed reports whether the principal may read an entity.
func (r *Registry) allowed(ctx context.Context, principal data.Principal, access string) (bool, error) {
	if r.auth == nil {
		return false, fmt.Errorf("%w: the entity registry has no authorizer", ErrForbidden)
	}
	level, err := r.auth.Level(ctx, principal, access)
	if err != nil {
		return false, fmt.Errorf("entities: checking %s on %s: %w", principal, access, err)
	}
	return data.Allows(level, rookeryv1.Level_LEVEL_VIEW), nil
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
