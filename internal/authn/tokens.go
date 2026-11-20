package authn

import (
	"context"
	"errors"
	"fmt"
	"strings"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/id"
	"github.com/aiviarium/rookery/internal/store"
)

// TokenPrefix starts every access token, so that a leaked one is recognisable
// and a scanner can refuse it before it reaches a log.
const TokenPrefix = "rkpat_"

// tokenSecretBytes is how much randomness follows the prefix: 32 bytes, 52
// base32 characters.
const tokenSecretBytes = 32

// Tokens are access tokens for scripts and the CLI.
type Tokens struct {
	store *store.Store
}

// NewTokens returns the access tokens of a database.
func NewTokens(opened *store.Store) *Tokens { return &Tokens{store: opened} }

// Create makes a token and returns its secret, which is shown once.
func (t *Tokens) Create(ctx context.Context, userID, name string) (string, store.RookeryTokens, error) {
	name = strings.TrimSpace(name)
	if name == "" {
		return "", store.RookeryTokens{}, fmt.Errorf("%w: a token needs a name", access.ErrInvalid)
	}
	secret := TokenPrefix + id.Secret(tokenSecretBytes)
	token, err := t.store.Queries().CreateToken(ctx, store.CreateTokenParams{
		ID:        id.New(),
		UserID:    userID,
		Name:      name,
		TokenHash: hash(secret),
	})
	if err != nil {
		return "", store.RookeryTokens{}, fmt.Errorf("authn: creating a token: %w", err)
	}
	return secret, token, nil
}

// Lookup returns the token a secret names, and marks it used. A secret that is
// not a token at all costs no query.
func (t *Tokens) Lookup(ctx context.Context, secret string) (store.RookeryTokens, error) {
	if !strings.HasPrefix(secret, TokenPrefix) || len(secret) <= len(TokenPrefix) {
		return store.RookeryTokens{}, fmt.Errorf("%w: not an access token", access.ErrUnauthenticated)
	}
	token, err := t.store.Queries().GetTokenByHash(ctx, hash(secret))
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return store.RookeryTokens{}, fmt.Errorf("%w: the token is not known", access.ErrUnauthenticated)
	case err != nil:
		return store.RookeryTokens{}, fmt.Errorf("authn: reading a token: %w", err)
	}
	_ = t.store.Queries().TouchToken(ctx, token.ID)
	return token, nil
}

// List is a user's tokens, newest first.
func (t *Tokens) List(ctx context.Context, userID string) ([]store.RookeryTokens, error) {
	tokens, err := t.store.Queries().ListTokens(ctx, userID)
	if err != nil {
		return nil, fmt.Errorf("authn: listing tokens: %w", err)
	}
	return tokens, nil
}

// Revoke deletes one of a user's tokens.
func (t *Tokens) Revoke(ctx context.Context, userID, tokenID string) error {
	revoked, err := t.store.Queries().DeleteToken(ctx, store.DeleteTokenParams{ID: tokenID, UserID: userID})
	switch {
	case err != nil:
		return fmt.Errorf("authn: revoking the token %s: %w", tokenID, err)
	case revoked == 0:
		return fmt.Errorf("%w: no token %s", access.ErrNotFound, tokenID)
	}
	return nil
}
