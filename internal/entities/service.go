package entities

import (
	"context"
	"fmt"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/data"
)

// Service is the entity API.
type Service struct {
	registry *Registry
	caller   data.Caller
}

// NewService returns the entity API's service.
func NewService(registry *Registry, caller data.Caller) *Service {
	return &Service{registry: registry, caller: caller}
}

// Resolve returns one entity.
func (s *Service) Resolve(ctx context.Context, request *connect.Request[rookeryv1.ResolveRequest]) (*connect.Response[rookeryv1.ResolveResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	entity, err := s.registry.Resolve(ctx, principal, request.Msg.GetUri())
	if err != nil {
		return nil, data.ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.ResolveResponse{Entity: entityMessage(entity)}), nil
}

// ResolveMany returns the entities of several URIs that the caller may read.
func (s *Service) ResolveMany(ctx context.Context, request *connect.Request[rookeryv1.ResolveManyRequest]) (*connect.Response[rookeryv1.ResolveManyResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	found, err := s.registry.ResolveMany(ctx, principal, request.Msg.GetUris())
	if err != nil {
		return nil, data.ConnectError(err)
	}
	entities := make([]*rookeryv1.Entity, len(found))
	for i, entity := range found {
		entities[i] = entityMessage(entity)
	}
	return connect.NewResponse(&rookeryv1.ResolveManyResponse{Entities: entities}), nil
}

// Search finds entities by full text, by vector similarity, or both.
func (s *Service) Search(ctx context.Context, request *connect.Request[rookeryv1.SearchRequest]) (*connect.Response[rookeryv1.SearchResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}

	var embedding []float32
	if len(request.Msg.GetEmbeddingCbor()) > 0 {
		var numbers []float32
		if err := cborx.Unmarshal(request.Msg.GetEmbeddingCbor(), &numbers); err != nil {
			return nil, data.ConnectError(fmt.Errorf("%w: the embedding is not a list of numbers: %w", ErrInvalid, err))
		}
		embedding = numbers
	}

	found, err := s.registry.Search(ctx, principal, request.Msg.GetQuery(), embedding, int(request.Msg.GetLimit()))
	if err != nil {
		return nil, data.ConnectError(err)
	}
	entities := make([]*rookeryv1.Entity, len(found))
	for i, entity := range found {
		entities[i] = entityMessage(entity)
	}
	return connect.NewResponse(&rookeryv1.SearchResponse{Entities: entities}), nil
}

// principal reads the caller from the context.
func (s *Service) principal(ctx context.Context) (data.Principal, error) {
	if s.caller == nil {
		return data.Principal{}, connect.NewError(connect.CodeUnauthenticated, fmt.Errorf("entities: no caller is configured"))
	}
	principal, err := s.caller.Principal(ctx)
	if err != nil {
		return data.Principal{}, data.ConnectError(err)
	}
	return principal, nil
}

// entityMessage is an entity on the wire.
func entityMessage(entity Entity) *rookeryv1.Entity {
	return &rookeryv1.Entity{
		Uri:        entity.URI,
		Kind:       entity.Kind,
		Title:      entity.Title,
		Collection: entity.Collection,
	}
}
