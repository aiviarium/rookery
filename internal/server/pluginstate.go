package server

import (
	"context"
	"errors"
	"fmt"
	"math"
	"strconv"
	"strings"

	"connectrpc.com/connect"
	"github.com/jackc/pgx/v5"

	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/store"
)

func requirePlugin(ctx context.Context) (string, error) {
	plugin, ok := access.PluginIdentity(ctx)
	if !ok {
		return "", access.ToConnect(access.ErrDenied)
	}
	return plugin, nil
}

// GetSecret reads a secret declared by the authenticated plugin.
func (p *PluginServer) GetSecret(ctx context.Context, request *connect.Request[pluginv1.ServerServiceGetSecretRequest]) (*connect.Response[pluginv1.ServerServiceGetSecretResponse], error) {
	plugin, err := requirePlugin(ctx)
	if err != nil {
		return nil, err
	}
	declared, err := p.installer.Manifest(ctx, plugin)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	name := request.Msg.GetName()
	allowed := false
	for _, secret := range declared.Secrets {
		allowed = allowed || secret == name
	}
	if !allowed {
		return nil, access.ToConnect(access.ErrNotFound)
	}
	value, err := p.secrets.Value(ctx, name, "plugin/"+plugin)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&pluginv1.ServerServiceGetSecretResponse{Value: value}), nil
}

// GetSetting resolves a setting declared by the authenticated plugin.
func (p *PluginServer) GetSetting(ctx context.Context, request *connect.Request[pluginv1.ServerServiceGetSettingRequest]) (*connect.Response[pluginv1.ServerServiceGetSettingResponse], error) {
	plugin, err := requirePlugin(ctx)
	if err != nil {
		return nil, err
	}
	declared, err := p.installer.Manifest(ctx, plugin)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	name := request.Msg.GetName()
	if _, ok := declared.Settings[name]; !ok {
		return nil, access.ToConnect(access.ErrNotFound)
	}
	principal := access.MustPrincipal(ctx)
	space := access.SpaceFrom(ctx)
	if space != "" {
		if err := p.checker.Check(ctx, principal, access.SpaceURI(space), access.View); err != nil {
			return nil, access.ToConnect(err)
		}
	}
	value, err := p.settings.Get(ctx, plugin+"/"+name, principal, space)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&pluginv1.ServerServiceGetSettingResponse{ValueCbor: value.Value, Scope: value.Scope}), nil
}

// GetKV reads private plugin state and its revision.
func (p *PluginServer) GetKV(ctx context.Context, request *connect.Request[pluginv1.ServerServiceGetKVRequest]) (*connect.Response[pluginv1.ServerServiceGetKVResponse], error) {
	plugin, err := requirePlugin(ctx)
	if err != nil {
		return nil, err
	}
	key := request.Msg.GetKey()
	if err := validatePluginKey(key); err != nil {
		return nil, access.ToConnect(err)
	}
	row, err := p.store.Queries().GetPluginKV(ctx, store.GetPluginKVParams{Plugin: plugin, Key: key})
	if err != nil {
		return nil, access.ToConnect(fmt.Errorf("plugin state: reading key: %w", err))
	}
	return connect.NewResponse(&pluginv1.ServerServiceGetKVResponse{ValueCbor: row.Value, Revision: row.Revision}), nil
}

// SetKV writes private plugin state, optionally comparing its revision first.
func (p *PluginServer) SetKV(ctx context.Context, request *connect.Request[pluginv1.ServerServiceSetKVRequest]) (*connect.Response[pluginv1.ServerServiceSetKVResponse], error) {
	plugin, err := requirePlugin(ctx)
	if err != nil {
		return nil, err
	}
	key, value := request.Msg.GetKey(), request.Msg.GetValueCbor()
	if err := validatePluginKey(key); err != nil {
		return nil, access.ToConnect(err)
	}
	if err := validatePluginCBOR(value); err != nil {
		return nil, access.ToConnect(err)
	}
	queries := p.store.Queries()
	var revision int64
	switch expected := request.Msg.ExpectedRevision; {
	case expected == nil:
		revision, err = queries.PutPluginKV(ctx, store.PutPluginKVParams{Plugin: plugin, Key: key, Value: value})
	case *expected == 0:
		revision, err = queries.CreatePluginKV(ctx, store.CreatePluginKVParams{Plugin: plugin, Key: key, Value: value})
	case *expected > 0:
		revision, err = queries.CompareAndSetPluginKV(ctx, store.CompareAndSetPluginKVParams{Plugin: plugin, Key: key, Value: value, Revision: *expected})
	default:
		return nil, access.ToConnect(fmt.Errorf("%w: negative revision", access.ErrInvalid))
	}
	if errors.Is(err, pgx.ErrNoRows) {
		return nil, access.ToConnect(fmt.Errorf("%w: the key's revision changed", access.ErrConflict))
	}
	if err != nil {
		return nil, access.ToConnect(fmt.Errorf("plugin state: writing key: %w", err))
	}
	return connect.NewResponse(&pluginv1.ServerServiceSetKVResponse{Revision: revision}), nil
}

func validatePluginKey(key string) error {
	if key == "" || len(key) > 512 || strings.ContainsRune(key, 0) {
		return fmt.Errorf("%w: expected a key between 1 and 512 bytes", access.ErrInvalid)
	}
	return nil
}

func validatePluginCBOR(value []byte) error {
	var decoded any
	if err := cborx.Unmarshal(value, &decoded); err != nil {
		return fmt.Errorf("%w: expected an encoded CBOR value", access.ErrInvalid)
	}
	return nil
}

// RegisterEntity adds or updates a plugin-owned entity.
func (p *PluginServer) RegisterEntity(ctx context.Context, request *connect.Request[pluginv1.ServerServiceRegisterEntityRequest]) (*connect.Response[pluginv1.ServerServiceRegisterEntityResponse], error) {
	plugin, err := requirePlugin(ctx)
	if err != nil {
		return nil, err
	}
	message := request.Msg
	key, owned := strings.CutPrefix(message.GetUri(), "entity://"+plugin+"/")
	if !owned || key == "" || len(key) > 512 || strings.ContainsAny(key, "\x00?#") || message.GetKind() == "" || message.GetTitle() == "" {
		return nil, access.ToConnect(fmt.Errorf("%w: an entity needs a plugin-owned URI, kind, and title", access.ErrInvalid))
	}
	resource := message.GetAccess()
	if resource == "" {
		resource = access.PluginURI(plugin)
	}
	if resource != access.PluginURI(plugin) {
		if err := p.checker.Check(ctx, access.MustPrincipal(ctx), resource, access.Use); err != nil {
			return nil, access.ToConnect(err)
		}
	}
	embedding, err := entityEmbedding(message.GetEmbedding())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	if err := p.store.Queries().UpsertPluginEntity(ctx, store.UpsertPluginEntityParams{
		Uri: message.GetUri(), Kind: message.GetKind(), Title: message.GetTitle(),
		Body: message.GetText(), Access: resource, Embedding: embedding,
	}); err != nil {
		return nil, access.ToConnect(fmt.Errorf("plugins: recording entity: %w", err))
	}
	return connect.NewResponse(&pluginv1.ServerServiceRegisterEntityResponse{}), nil
}

func entityEmbedding(values []float32) (*string, error) {
	if len(values) == 0 {
		return nil, nil
	}
	if len(values) > 16000 {
		return nil, fmt.Errorf("%w: embedding is too large", access.ErrInvalid)
	}
	parts := make([]string, len(values))
	for index, value := range values {
		if math.IsNaN(float64(value)) || math.IsInf(float64(value), 0) {
			return nil, fmt.Errorf("%w: embedding values must be finite", access.ErrInvalid)
		}
		parts[index] = strconv.FormatFloat(float64(value), 'g', -1, 32)
	}
	value := "[" + strings.Join(parts, ",") + "]"
	return &value, nil
}

// DatabaseCredentials opens the SQL role reserved for this plugin.
func (p *PluginServer) DatabaseCredentials(ctx context.Context, _ *connect.Request[pluginv1.ServerServiceDatabaseCredentialsRequest]) (*connect.Response[pluginv1.ServerServiceDatabaseCredentialsResponse], error) {
	plugin, err := requirePlugin(ctx)
	if err != nil {
		return nil, err
	}
	manifest, err := p.installer.Manifest(ctx, plugin)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	if manifest.Database != "sql" {
		return nil, access.ToConnect(access.ErrDenied)
	}
	dsn, err := p.datastore.SQLCredentials(ctx, plugin)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&pluginv1.ServerServiceDatabaseCredentialsResponse{Dsn: dsn}), nil
}
