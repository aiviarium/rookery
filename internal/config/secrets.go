package config

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"time"

	"connectrpc.com/connect"
	"github.com/jackc/pgx/v5"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/seal"
	"github.com/aiviarium/rookery/internal/store"
)

// Secret scopes, in the form the database holds: "global", "plugin/<id>",
// "provisioner/<name>", "host/<name>".
const (
	// SecretScopeGlobal is the deployment's own: the apply and the server read
	// it.
	SecretScopeGlobal = "global"
	// SecretScopePlugin is a plugin's own secrets.
	SecretScopePlugin = "plugin/"
	// SecretScopeProvisioner is a provisioner entry's.
	SecretScopeProvisioner = "provisioner/"
	// SecretScopeHost is a host's, which its `box secret` reads.
	SecretScopeHost = "host/"
)

// Secrets are the deployment's secrets: sealed with the deployment key, and
// readable only through the server.
type Secrets struct {
	store *store.Store
	ring  *seal.Ring
}

// NewSecrets returns the secrets of a database, sealed with a ring.
func NewSecrets(opened *store.Store, ring *seal.Ring) *Secrets {
	return &Secrets{store: opened, ring: ring}
}

// Secret is a secret's name, scope and dates, never its value.
type Secret struct {
	Name      string
	Scope     string
	CreatedAt time.Time
	UpdatedAt time.Time
}

// Message is a secret as the edge API has it.
func (s Secret) Message() *rookeryv1.Secret {
	out := &rookeryv1.Secret{Name: s.Name, Scope: s.Scope}
	if !s.CreatedAt.IsZero() {
		out.CreatedAt = timestamppb.New(s.CreatedAt)
	}
	if !s.UpdatedAt.IsZero() {
		out.UpdatedAt = timestamppb.New(s.UpdatedAt)
	}
	return out
}

// secretOf is a secret from the database.
func secretOf(record store.RookerySecrets) Secret {
	secret := Secret{Name: record.Name, Scope: record.Scope, CreatedAt: record.CreatedAt}
	secret.UpdatedAt = record.UpdatedAt
	return secret
}

// Set writes a secret, sealed.
func (s *Secrets) Set(ctx context.Context, name, scope string, value []byte) (Secret, error) {
	name = strings.TrimSpace(name)
	scope = strings.TrimSpace(scope)
	if name == "" {
		return Secret{}, fmt.Errorf("%w: a secret needs a name", access.ErrInvalid)
	}
	if len(value) == 0 {
		return Secret{}, fmt.Errorf("%w: the secret %s has no value", access.ErrInvalid, name)
	}
	if err := checkSecretScope(scope); err != nil {
		return Secret{}, err
	}

	sealed, keyID, err := s.ring.Seal(value, secretAssociatedData(name, scope))
	if err != nil {
		return Secret{}, err
	}
	record, err := s.store.Queries().UpsertSecret(ctx, store.UpsertSecretParams{
		Name:  name,
		Scope: scope,
		Value: sealed,
		KeyID: keyID,
	})
	if err != nil {
		return Secret{}, fmt.Errorf("config: sealing the secret %s: %w", name, err)
	}
	return secretOf(record), nil
}

// Value opens a secret. It is what the apply, a provisioner and a plugin get,
// and never a caller through the edge.
func (s *Secrets) Value(ctx context.Context, name, scope string) ([]byte, error) {
	record, err := s.store.Queries().GetSecret(ctx, store.GetSecretParams{Name: name, Scope: scope})
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return nil, fmt.Errorf("%w: there is no secret %s in %s", access.ErrNotFound, name, scope)
	case err != nil:
		return nil, fmt.Errorf("config: reading the secret %s: %w", name, err)
	}
	value, err := s.ring.Open(record.Value, record.KeyID, secretAssociatedData(name, scope))
	if err != nil {
		return nil, fmt.Errorf("config: opening the secret %s: %w", name, err)
	}
	return value, nil
}

// List returns the secrets' names and scopes, never their values.
func (s *Secrets) List(ctx context.Context, scope string) ([]Secret, error) {
	records, err := s.store.Queries().ListSecrets(ctx, optional(strings.TrimSpace(scope)))
	if err != nil {
		return nil, fmt.Errorf("config: listing secrets: %w", err)
	}
	out := make([]Secret, 0, len(records))
	for _, record := range records {
		out = append(out, secretOf(record))
	}
	return out, nil
}

// Delete removes a secret.
func (s *Secrets) Delete(ctx context.Context, name, scope string) error {
	deleted, err := s.store.Queries().DeleteSecret(ctx, store.DeleteSecretParams{Name: name, Scope: scope})
	switch {
	case err != nil:
		return fmt.Errorf("config: deleting the secret %s: %w", name, err)
	case deleted == 0:
		return fmt.Errorf("%w: there is no secret %s in %s", access.ErrNotFound, name, scope)
	}
	return nil
}

// Rename gives a secret another name, keeping its value and its scope.
func (s *Secrets) Rename(ctx context.Context, name, scope, newName string) (Secret, error) {
	newName = strings.TrimSpace(newName)
	if newName == "" {
		return Secret{}, fmt.Errorf("%w: a secret needs a name", access.ErrInvalid)
	}

	record, err := s.store.Queries().GetSecret(ctx, store.GetSecretParams{Name: name, Scope: scope})
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return Secret{}, fmt.Errorf("%w: there is no secret %s in %s", access.ErrNotFound, name, scope)
	case err != nil:
		return Secret{}, fmt.Errorf("config: reading the secret %s: %w", name, err)
	}

	// The associated data holds the name, so the value is resealed under the new
	// one.
	value, err := s.ring.Open(record.Value, record.KeyID, secretAssociatedData(name, scope))
	if err != nil {
		return Secret{}, fmt.Errorf("config: opening the secret %s: %w", name, err)
	}
	renamed, err := s.Set(ctx, newName, scope, value)
	if err != nil {
		return Secret{}, err
	}
	if err := s.Delete(ctx, name, scope); err != nil {
		return Secret{}, err
	}
	return renamed, nil
}

// Rekey reseals every secret and every connection under the current key, and
// says how many values it rewrote.
func (s *Secrets) Rekey(ctx context.Context) (int64, error) {
	var resealed int64

	secrets, err := s.store.Queries().ListSecretsForRekey(ctx)
	if err != nil {
		return 0, fmt.Errorf("config: listing secrets for a rekey: %w", err)
	}
	for _, record := range secrets {
		value, keyID, changed, err := s.ring.Rekey(record.Value, record.KeyID, secretAssociatedData(record.Name, record.Scope))
		if err != nil {
			return 0, fmt.Errorf("config: resealing the secret %s: %w", record.Name, err)
		}
		if !changed {
			continue
		}
		if err := s.store.Queries().ResealSecret(ctx, store.ResealSecretParams{
			Name:  record.Name,
			Scope: record.Scope,
			Value: value,
			KeyID: keyID,
		}); err != nil {
			return 0, fmt.Errorf("config: resealing the secret %s: %w", record.Name, err)
		}
		resealed++
	}

	connections, err := s.store.Queries().ListConnectionsForRekey(ctx)
	if err != nil {
		return 0, fmt.Errorf("config: listing connections for a rekey: %w", err)
	}
	for _, record := range connections {
		value, keyID, changed, err := s.ring.Rekey(record.Values, record.KeyID, []byte(record.ID))
		if err != nil {
			return 0, fmt.Errorf("config: resealing the connection %s: %w", record.Name, err)
		}
		if !changed {
			continue
		}
		if err := s.store.Queries().ResealConnection(ctx, store.ResealConnectionParams{
			ID:     record.ID,
			Values: value,
			KeyID:  keyID,
		}); err != nil {
			return 0, fmt.Errorf("config: resealing the connection %s: %w", record.Name, err)
		}
		resealed++
	}
	return resealed, nil
}

// secretAssociatedData binds a sealed secret to its name and scope.
func secretAssociatedData(name, scope string) []byte {
	return []byte(name + "\x00" + scope)
}

// checkSecretScope refuses a scope nothing can read from.
func checkSecretScope(scope string) error {
	switch {
	case scope == SecretScopeGlobal:
		return nil
	case strings.HasPrefix(scope, SecretScopePlugin) && len(scope) > len(SecretScopePlugin):
		return nil
	case strings.HasPrefix(scope, SecretScopeProvisioner) && len(scope) > len(SecretScopeProvisioner):
		return nil
	case strings.HasPrefix(scope, SecretScopeHost) && len(scope) > len(SecretScopeHost):
		return nil
	default:
		return fmt.Errorf("%w: %q is not a secret scope: global, plugin/<id>, provisioner/<name> or host/<name>", access.ErrInvalid, scope)
	}
}

// SecretsService is SecretService: admins write secrets and list their names. A
// value never crosses the edge.
type SecretsService struct {
	secrets *Secrets
}

// NewSecretsService returns SecretService for a database.
func NewSecretsService(secrets *Secrets) *SecretsService { return &SecretsService{secrets: secrets} }

// List returns the secrets' names and scopes.
func (s *SecretsService) List(ctx context.Context, request *connect.Request[rookeryv1.SecretServiceListRequest]) (*connect.Response[rookeryv1.SecretServiceListResponse], error) {
	found, err := s.secrets.List(ctx, request.Msg.GetScope())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := make([]*rookeryv1.Secret, 0, len(found))
	for _, secret := range found {
		out = append(out, secret.Message())
	}
	return connect.NewResponse(&rookeryv1.SecretServiceListResponse{Secrets: out}), nil
}

// Set writes a secret.
func (s *SecretsService) Set(ctx context.Context, request *connect.Request[rookeryv1.SecretServiceSetRequest]) (*connect.Response[rookeryv1.SecretServiceSetResponse], error) {
	secret, err := s.secrets.Set(ctx, request.Msg.GetName(), request.Msg.GetScope(), request.Msg.GetValue())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.SecretServiceSetResponse{Secret: secret.Message()}), nil
}

// Delete removes a secret.
func (s *SecretsService) Delete(ctx context.Context, request *connect.Request[rookeryv1.SecretServiceDeleteRequest]) (*connect.Response[rookeryv1.SecretServiceDeleteResponse], error) {
	if err := s.secrets.Delete(ctx, request.Msg.GetName(), request.Msg.GetScope()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.SecretServiceDeleteResponse{}), nil
}

// Rename gives a secret another name.
func (s *SecretsService) Rename(ctx context.Context, request *connect.Request[rookeryv1.SecretServiceRenameRequest]) (*connect.Response[rookeryv1.SecretServiceRenameResponse], error) {
	secret, err := s.secrets.Rename(ctx, request.Msg.GetName(), request.Msg.GetScope(), request.Msg.GetNewName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.SecretServiceRenameResponse{Secret: secret.Message()}), nil
}

// optional is a filter value, or nil for no filter.
func optional(value string) *string {
	if value == "" {
		return nil
	}
	return &value
}
