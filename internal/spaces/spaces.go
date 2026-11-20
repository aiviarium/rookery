// Package spaces is containers: a kind the core or a plugin declares, a name, and
// the records and documents scoped to it.
//
// A space is the resource space://ID, and whoever makes one holds manage on it.
// The caller's current space, the Rookery-Space header, is what the datastore
// scopes a query to.
package spaces

import (
	"context"
	"errors"
	"fmt"
	"sort"
	"strings"
	"sync"

	"connectrpc.com/connect"
	"github.com/jackc/pgx/v5"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/id"
	"github.com/aiviarium/rookery/internal/store"
)

// Kind is a kind of space. The core declares some; a plugin's manifest declares
// more, and they arrive with the plugin system.
type Kind struct {
	ID     string
	Label  string
	Plugin string
}

// coreKinds are the kinds the core declares.
var coreKinds = []Kind{
	{ID: "general", Label: "General"},
	{ID: "project", Label: "Project"},
}

// CoreKinds are the kinds the core declares.
func CoreKinds() []Kind {
	out := make([]Kind, len(coreKinds))
	copy(out, coreKinds)
	return out
}

// Registry are the kinds of space a deployment has: the core's, and the ones the
// installed plugins declare.
type Registry struct {
	mu       sync.RWMutex
	declared []Kind
}

// NewRegistry returns the core's kinds, with none declared yet.
func NewRegistry() *Registry { return &Registry{} }

// Declare adds the kinds a plugin declares. It refuses a kind of the core's and
// one that is declared already, so that a space's kind always has one owner.
func (r *Registry) Declare(kinds ...Kind) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	for _, kind := range kinds {
		if kind.ID == "" || kind.Plugin == "" {
			return fmt.Errorf("%w: a kind of space needs an id and its plugin", access.ErrInvalid)
		}
		if IsCoreKind(kind.ID) {
			return fmt.Errorf("%w: %s is a kind of the core's", access.ErrInvalid, kind.ID)
		}
		for _, declared := range r.declared {
			if declared.ID == kind.ID {
				return fmt.Errorf("%w: the kind %s is declared by %s", access.ErrConflict, kind.ID, declared.Plugin)
			}
		}
		r.declared = append(r.declared, kind)
	}
	return nil
}

// Replace validates and swaps the kinds belonging to a plugin.
func (r *Registry) Replace(plugin string, kinds ...Kind) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if err := r.check(plugin, kinds); err != nil {
		return err
	}
	next := make([]Kind, 0, len(r.declared)+len(kinds))
	for _, kind := range r.declared {
		if kind.Plugin != plugin {
			next = append(next, kind)
		}
	}
	r.declared = append(next, kinds...)
	return nil
}

// CheckReplace checks a plugin's kinds before an install changes state.
func (r *Registry) CheckReplace(plugin string, kinds ...Kind) error {
	r.mu.RLock()
	defer r.mu.RUnlock()
	return r.check(plugin, kinds)
}

func (r *Registry) check(plugin string, kinds []Kind) error {
	for _, kind := range kinds {
		if kind.ID == "" || kind.Plugin != plugin || plugin == "" || IsCoreKind(kind.ID) {
			return fmt.Errorf("%w: invalid declaration of space kind %q", access.ErrInvalid, kind.ID)
		}
		for _, declared := range r.declared {
			if declared.ID == kind.ID && declared.Plugin != plugin {
				return fmt.Errorf("%w: the space kind %s is declared by %s", access.ErrConflict, kind.ID, declared.Plugin)
			}
		}
	}
	return nil
}

// Has reports whether a space kind is available for creation.
func (r *Registry) Has(kind string) bool {
	if IsCoreKind(kind) {
		return true
	}
	r.mu.RLock()
	defer r.mu.RUnlock()
	for _, declared := range r.declared {
		if declared.ID == kind {
			return true
		}
	}
	return false
}

// All returns the core's kinds and then the plugins', by id.
func (r *Registry) All() []Kind {
	r.mu.RLock()
	defer r.mu.RUnlock()
	out := CoreKinds()
	rest := make([]Kind, len(r.declared))
	copy(rest, r.declared)
	sort.Slice(rest, func(i, j int) bool { return rest[i].ID < rest[j].ID })
	return append(out, rest...)
}

// IsCoreKind reports whether a kind is one of the core's.
func IsCoreKind(kind string) bool {
	for _, core := range coreKinds {
		if core.ID == kind {
			return true
		}
	}
	return false
}

// Spaces reads and writes spaces.
type Spaces struct {
	store   *store.Store
	checker *access.Checker
}

// New returns the spaces of a database.
func New(opened *store.Store, checker *access.Checker) *Spaces {
	return &Spaces{store: opened, checker: checker}
}

// List is a page of spaces the caller may view, by id. An admin sees every
// space.
func (s *Spaces) List(ctx context.Context, principal access.Principal, kind string, includeArchived bool, cursor string, limit int) ([]store.RookerySpaces, error) {
	if !principal.Authenticated() {
		return nil, access.ErrUnauthenticated
	}

	var found []store.RookerySpaces
	var err error
	if principal.IsAdmin() {
		params := store.ListSpacesParams{
			ID:              cursor,
			IncludeArchived: includeArchived,
			RowLimit:        clampLimit(limit),
		}
		if kind != "" {
			params.Kind = &kind
		}
		found, err = s.store.Queries().ListSpaces(ctx, params)
	} else {
		principals, groupsErr := s.principalsOf(ctx, principal)
		if groupsErr != nil {
			return nil, groupsErr
		}
		params := store.ListSpacesVisibleToParams{
			Cursor:          cursor,
			IncludeArchived: includeArchived,
			Principals:      principals,
			RowLimit:        clampLimit(limit),
		}
		if kind != "" {
			params.Kind = &kind
		}
		found, err = s.store.Queries().ListSpacesVisibleTo(ctx, params)
	}
	if err != nil {
		return nil, fmt.Errorf("spaces: listing spaces: %w", err)
	}
	return found, nil
}

// principalsOf is the grant principals the caller matches.
func (s *Spaces) principalsOf(ctx context.Context, principal access.Principal) ([]string, error) {
	principals := []string{principal.String(), access.Everyone}
	groups, err := s.store.Queries().ListGroupsForUser(ctx, principal.ID)
	if err != nil {
		return nil, fmt.Errorf("spaces: reading the caller's groups: %w", err)
	}
	for _, group := range groups {
		principals = append(principals, access.GroupPrincipal(group.ID))
	}
	return principals, nil
}

// ByID returns a space, or access.ErrNotFound.
func (s *Spaces) ByID(ctx context.Context, spaceID string) (store.RookerySpaces, error) {
	space, err := s.store.Queries().GetSpace(ctx, spaceID)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return store.RookerySpaces{}, fmt.Errorf("%w: no space %s", access.ErrNotFound, spaceID)
	case err != nil:
		return store.RookerySpaces{}, fmt.Errorf("spaces: reading the space %s: %w", spaceID, err)
	}
	return space, nil
}

// Create makes a space and grants its creator manage on it.
func (s *Spaces) Create(ctx context.Context, creatorID, kind, name string) (store.RookerySpaces, error) {
	return s.create(ctx, creatorID, kind, name, IsCoreKind)
}

func (s *Spaces) create(ctx context.Context, creatorID, kind, name string, known func(string) bool) (store.RookerySpaces, error) {
	kind = strings.TrimSpace(kind)
	name = strings.TrimSpace(name)
	switch {
	case !known(kind):
		return store.RookerySpaces{}, fmt.Errorf("%w: %q is not a space kind this deployment declares", access.ErrInvalid, kind)
	case name == "":
		return store.RookerySpaces{}, fmt.Errorf("%w: a space needs a name", access.ErrInvalid)
	}

	tx, err := s.store.Pool().Begin(ctx)
	if err != nil {
		return store.RookerySpaces{}, fmt.Errorf("spaces: beginning the create: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	queries := s.store.Queries().WithTx(tx)
	space, err := queries.CreateSpace(ctx, store.CreateSpaceParams{
		ID:        id.New(),
		Kind:      kind,
		Name:      name,
		CreatedBy: &creatorID,
	})
	if err != nil {
		if isUniqueViolation(err) {
			return store.RookerySpaces{}, fmt.Errorf("%w: a %s space called %s is there already", access.ErrConflict, kind, name)
		}
		return store.RookerySpaces{}, fmt.Errorf("spaces: creating the space %s: %w", name, err)
	}
	if err := queries.UpsertResource(ctx, store.UpsertResourceParams{Uri: access.SpaceURI(space.ID)}); err != nil {
		return store.RookerySpaces{}, fmt.Errorf("spaces: recording the new space's resource: %w", err)
	}
	if _, err := queries.UpsertGrant(ctx, store.UpsertGrantParams{
		Resource:  access.SpaceURI(space.ID),
		Principal: access.UserPrincipal(creatorID),
		Level:     int16(access.Manage),
	}); err != nil {
		return store.RookerySpaces{}, fmt.Errorf("spaces: granting manage on the new space: %w", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return store.RookerySpaces{}, fmt.Errorf("spaces: committing the create: %w", err)
	}
	return space, nil
}

// Rename changes a space's name.
func (s *Spaces) Rename(ctx context.Context, spaceID, name string) (store.RookerySpaces, error) {
	name = strings.TrimSpace(name)
	if name == "" {
		return store.RookerySpaces{}, fmt.Errorf("%w: a space needs a name", access.ErrInvalid)
	}
	if _, err := s.ByID(ctx, spaceID); err != nil {
		return store.RookerySpaces{}, err
	}
	space, err := s.store.Queries().RenameSpace(ctx, store.RenameSpaceParams{ID: spaceID, Name: name})
	if err != nil {
		if isUniqueViolation(err) {
			return store.RookerySpaces{}, fmt.Errorf("%w: a space called %s is there already", access.ErrConflict, name)
		}
		return store.RookerySpaces{}, fmt.Errorf("spaces: renaming the space %s: %w", spaceID, err)
	}
	return space, nil
}

// Archive archives or unarchives a space. An archived space keeps its records and
// its grants, and lists as archived.
func (s *Spaces) Archive(ctx context.Context, spaceID string, archived bool) (store.RookerySpaces, error) {
	if _, err := s.ByID(ctx, spaceID); err != nil {
		return store.RookerySpaces{}, err
	}
	space, err := s.store.Queries().ArchiveSpace(ctx, store.ArchiveSpaceParams{ID: spaceID, Archived: archived})
	if err != nil {
		return store.RookerySpaces{}, fmt.Errorf("spaces: archiving the space %s: %w", spaceID, err)
	}
	return space, nil
}

// Delete removes a space, its resource and its grants. Its records arrive with
// the datastore, which deletes them first.
func (s *Spaces) Delete(ctx context.Context, spaceID string) error {
	if _, err := s.ByID(ctx, spaceID); err != nil {
		return err
	}
	tx, err := s.store.Pool().Begin(ctx)
	if err != nil {
		return fmt.Errorf("spaces: beginning the delete: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	queries := s.store.Queries().WithTx(tx)
	if _, err := queries.DeleteSpace(ctx, spaceID); err != nil {
		return fmt.Errorf("spaces: deleting the space %s: %w", spaceID, err)
	}
	if _, err := queries.DeleteResource(ctx, access.SpaceURI(spaceID)); err != nil {
		return fmt.Errorf("spaces: deleting the resource of %s: %w", spaceID, err)
	}
	if err := tx.Commit(ctx); err != nil {
		return fmt.Errorf("spaces: committing the delete: %w", err)
	}
	return nil
}

// Message is a space as the edge API has it.
func Message(space store.RookerySpaces) *rookeryv1.Space {
	out := &rookeryv1.Space{
		Id:       space.ID,
		Kind:     space.Kind,
		Name:     space.Name,
		Archived: space.Archived,
	}
	if space.CreatedBy != nil {
		out.CreatedBy = *space.CreatedBy
	}
	if !space.CreatedAt.IsZero() {
		out.CreatedAt = timestamppb.New(space.CreatedAt)
	}
	if !space.UpdatedAt.IsZero() {
		out.UpdatedAt = timestamppb.New(space.UpdatedAt)
	}
	return out
}

// Service is SpaceService.
type Service struct {
	spaces   *Spaces
	registry *Registry
}

// NewService returns SpaceService for a database and the deployment's kinds.
func NewService(open *Spaces, registry *Registry) *Service {
	if registry == nil {
		registry = NewRegistry()
	}
	return &Service{spaces: open, registry: registry}
}

// ListKinds returns the kinds the core declares.
func (s *Service) ListKinds(context.Context, *connect.Request[rookeryv1.SpaceServiceListKindsRequest]) (*connect.Response[rookeryv1.SpaceServiceListKindsResponse], error) {
	kinds := s.registry.All()
	out := make([]*rookeryv1.SpaceKind, 0, len(kinds))
	for _, kind := range kinds {
		out = append(out, &rookeryv1.SpaceKind{Id: kind.ID, Label: kind.Label, Plugin: kind.Plugin})
	}
	return connect.NewResponse(&rookeryv1.SpaceServiceListKindsResponse{Kinds: out}), nil
}

// List returns a page of spaces.
func (s *Service) List(ctx context.Context, request *connect.Request[rookeryv1.SpaceServiceListRequest]) (*connect.Response[rookeryv1.SpaceServiceListResponse], error) {
	principal := access.MustPrincipal(ctx)
	found, err := s.spaces.List(ctx, principal, request.Msg.GetKind(), request.Msg.GetIncludeArchived(), request.Msg.GetCursor(), int(request.Msg.GetLimit()))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := make([]*rookeryv1.Space, 0, len(found))
	for _, space := range found {
		out = append(out, Message(space))
	}
	next := ""
	limit := int(request.Msg.GetLimit())
	if limit <= 0 {
		limit = 100
	}
	if len(found) == limit {
		next = found[len(found)-1].ID
	}
	return connect.NewResponse(&rookeryv1.SpaceServiceListResponse{Spaces: out, NextCursor: next}), nil
}

// Get returns one space.
func (s *Service) Get(ctx context.Context, request *connect.Request[rookeryv1.SpaceServiceGetRequest]) (*connect.Response[rookeryv1.SpaceServiceGetResponse], error) {
	space, err := s.spaces.ByID(ctx, request.Msg.GetSpace())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.SpaceServiceGetResponse{Space: Message(space)}), nil
}

// Create makes a space.
func (s *Service) Create(ctx context.Context, request *connect.Request[rookeryv1.SpaceServiceCreateRequest]) (*connect.Response[rookeryv1.SpaceServiceCreateResponse], error) {
	principal := access.MustPrincipal(ctx)
	space, err := s.spaces.create(ctx, principal.ID, request.Msg.GetKind(), request.Msg.GetName(), s.registry.Has)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.SpaceServiceCreateResponse{Space: Message(space)}), nil
}

// Rename changes a space's name.
func (s *Service) Rename(ctx context.Context, request *connect.Request[rookeryv1.SpaceServiceRenameRequest]) (*connect.Response[rookeryv1.SpaceServiceRenameResponse], error) {
	space, err := s.spaces.Rename(ctx, request.Msg.GetSpace(), request.Msg.GetName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.SpaceServiceRenameResponse{Space: Message(space)}), nil
}

// Archive archives or unarchives a space.
func (s *Service) Archive(ctx context.Context, request *connect.Request[rookeryv1.SpaceServiceArchiveRequest]) (*connect.Response[rookeryv1.SpaceServiceArchiveResponse], error) {
	space, err := s.spaces.Archive(ctx, request.Msg.GetSpace(), request.Msg.GetArchived())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.SpaceServiceArchiveResponse{Space: Message(space)}), nil
}

// Delete removes a space.
func (s *Service) Delete(ctx context.Context, request *connect.Request[rookeryv1.SpaceServiceDeleteRequest]) (*connect.Response[rookeryv1.SpaceServiceDeleteResponse], error) {
	if err := s.spaces.Delete(ctx, request.Msg.GetSpace()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.SpaceServiceDeleteResponse{}), nil
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
