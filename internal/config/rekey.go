package config

import (
	"context"
	"errors"
	"fmt"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/seal"
	"github.com/aiviarium/rookery/internal/store"
)

func (s *Secrets) rekeyInfrastructure(ctx context.Context, q *store.Queries) (int64, error) {
	keys, err := q.ListSSHKeysForRekey(ctx)
	if err != nil {
		return 0, fmt.Errorf("config: listing SSH keys: %w", err)
	}
	roles, err := q.ListSQLRolesForRekey(ctx)
	if err != nil {
		return 0, fmt.Errorf("config: listing SQL credentials: %w", err)
	}
	var count int64
	for _, key := range keys {
		value, keyID, changed, err := s.ring.Rekey(key.PrivateKey, key.KeyID, []byte(key.ID))
		if err != nil {
			return 0, fmt.Errorf("config: resealing SSH key %s: %w", key.ID, err)
		}
		if changed {
			if err := q.ResealSSHKey(ctx, store.ResealSSHKeyParams{ID: key.ID, PrivateKey: value, KeyID: keyID}); err != nil {
				return 0, fmt.Errorf("config: replacing sealing key for SSH key %s: %w", key.ID, err)
			}
			count++
		}
	}
	for _, role := range roles {
		value, keyID, changed, err := s.ring.Rekey(role.Password, role.KeyID, []byte(role.Plugin))
		if err != nil {
			return 0, fmt.Errorf("config: resealing SQL credentials for %s: %w", role.Plugin, err)
		}
		if changed {
			if err := q.ResealSQLRole(ctx, store.ResealSQLRoleParams{Plugin: role.Plugin, Password: value, KeyID: keyID}); err != nil {
				return 0, fmt.Errorf("config: replacing sealing key for SQL credentials: %w", err)
			}
			count++
		}
	}
	tailnet, err := q.GetTailnetConfig(ctx)
	if errors.Is(err, pgx.ErrNoRows) {
		return count, nil
	}
	if err != nil {
		return 0, fmt.Errorf("config: reading tailnet credentials: %w", err)
	}
	value, keyID, changed, err := s.ring.Rekey(tailnet.ApiKey, tailnet.KeyID, []byte(seal.TailnetControlAAD))
	if err != nil {
		return 0, fmt.Errorf("config: resealing tailnet credentials: %w", err)
	}
	if !changed {
		return count, nil
	}
	changedRows, err := q.ResealTailnetKey(ctx, store.ResealTailnetKeyParams{ApiKey: value, KeyID: keyID, OldKeyID: tailnet.KeyID})
	if err != nil {
		return 0, fmt.Errorf("config: replacing sealing key for tailnet credentials: %w", err)
	}
	return count + changedRows, nil
}
