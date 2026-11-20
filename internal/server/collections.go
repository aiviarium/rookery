package server

import (
	"context"
	"log/slog"
	"net/http"
	"time"

	"connectrpc.com/connect"

	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/data"
	"github.com/aiviarium/rookery/internal/docs"
	"github.com/aiviarium/rookery/internal/entities"
	"github.com/aiviarium/rookery/internal/seal"
	"github.com/aiviarium/rookery/internal/store"
)

type dataCaller struct{}

func (dataCaller) Principal(ctx context.Context) (data.Principal, error) {
	principal, ok := access.PrincipalFrom(ctx)
	if !ok {
		return data.Principal{}, access.ErrUnauthenticated
	}
	return principal, nil
}

type dataAccess struct {
	checker *access.Checker
}

func (a dataAccess) Level(ctx context.Context, principal data.Principal, resource string) (access.Level, error) {
	switch {
	case principal.IsLocal():
		return access.Local, nil
	case principal.IsAdmin():
		return access.Admin, nil
	case principal.Kind == access.KindUser && resource == "user://"+principal.ID:
		return access.Self, nil
	default:
		return a.checker.Level(ctx, principal, resource)
	}
}

type collectionServices struct {
	data     *data.Data
	docs     *docs.Documents
	entities *entities.Registry
}

func wireCollections(ctx context.Context, opened *store.Store, checker *access.Checker, ring *seal.Ring) (*collectionServices, error) {
	auth := dataAccess{checker: checker}
	datastore := data.New(data.Config{Store: opened, Authorizer: auth, DatabaseURL: opened.URL(), Sealer: ring})
	if err := datastore.Load(ctx); err != nil {
		return nil, err
	}
	return &collectionServices{
		data:     datastore,
		docs:     docs.New(docs.Config{Store: opened, Authorizer: auth}),
		entities: entities.New(entities.Config{Data: datastore, Authorizer: auth}),
	}, nil
}

func (s *collectionServices) register(mux *http.ServeMux, options []connect.HandlerOption) {
	caller := dataCaller{}
	path, handler := rookeryv1connect.NewDataServiceHandler(data.NewService(s.data, caller), options...)
	mux.Handle(path, handler)
	path, handler = rookeryv1connect.NewEntityServiceHandler(entities.NewService(s.entities, caller), options...)
	mux.Handle(path, handler)
	path, handler = rookeryv1connect.NewDocumentServiceHandler(docs.NewService(s.docs, caller), options...)
	mux.Handle(path, docs.WithRequestBody(handler))
}

func (h *handlers) collectExpiring(ctx context.Context) error {
	ticker := time.NewTicker(time.Hour)
	defer ticker.Stop()
	for {
		if _, err := h.collections.docs.Prune(ctx); err != nil && ctx.Err() == nil {
			h.log.Error("pruning document updates", slog.Any("error", err))
		}
		if _, err := h.blobs.Collect(ctx, 24*time.Hour); err != nil && ctx.Err() == nil {
			h.log.Error("collecting unused blobs", slog.Any("error", err))
		}
		select {
		case <-ctx.Done():
			return nil
		case <-ticker.C:
		}
	}
}
