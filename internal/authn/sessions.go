package authn

import (
	"context"
	"errors"
	"fmt"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/id"
	"github.com/aiviarium/rookery/internal/store"
)

// cookieSecretBytes is how much randomness a session's secret holds: 32 bytes,
// 52 base32 characters.
const cookieSecretBytes = 32

// Sessions are device sessions: a browser's cookie, a CSRF token it echoes in a
// header, and the user behind both.
type Sessions struct {
	store *store.Store
}

// NewSessions returns the sessions of a database.
func NewSessions(opened *store.Store) *Sessions { return &Sessions{store: opened} }

// Create makes a session and returns the secret that goes in the cookie.
func (s *Sessions) Create(ctx context.Context, userID, userAgent string) (string, store.RookerySessions, error) {
	secret := id.Secret(cookieSecretBytes)
	session, err := s.store.Queries().CreateSession(ctx, store.CreateSessionParams{
		ID:         id.New(),
		SecretHash: hash(secret),
		UserID:     userID,
		Csrf:       id.Secret(cookieSecretBytes),
		UserAgent:  userAgent,
	})
	if err != nil {
		return "", store.RookerySessions{}, fmt.Errorf("authn: creating a session: %w", err)
	}
	return secret, session, nil
}

// FromSecret returns the session a cookie names, and marks it seen.
func (s *Sessions) FromSecret(ctx context.Context, secret string) (store.RookerySessions, error) {
	if secret == "" {
		return store.RookerySessions{}, fmt.Errorf("%w: no session", access.ErrUnauthenticated)
	}
	session, err := s.store.Queries().GetSessionByHash(ctx, hash(secret))
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return store.RookerySessions{}, fmt.Errorf("%w: the session is gone", access.ErrUnauthenticated)
	case err != nil:
		return store.RookerySessions{}, fmt.Errorf("authn: reading a session: %w", err)
	}
	// A failed touch is not worth failing a signed-in call over.
	_ = s.store.Queries().TouchSession(ctx, session.ID)
	return session, nil
}

// List is a user's sessions, newest first.
func (s *Sessions) List(ctx context.Context, userID string) ([]store.RookerySessions, error) {
	sessions, err := s.store.Queries().ListSessions(ctx, userID)
	if err != nil {
		return nil, fmt.Errorf("authn: listing sessions: %w", err)
	}
	return sessions, nil
}

// Revoke ends one of a user's sessions.
func (s *Sessions) Revoke(ctx context.Context, userID, sessionID string) error {
	revoked, err := s.store.Queries().DeleteSession(ctx, store.DeleteSessionParams{ID: sessionID, UserID: userID})
	switch {
	case err != nil:
		return fmt.Errorf("authn: revoking the session %s: %w", sessionID, err)
	case revoked == 0:
		return fmt.Errorf("%w: no session %s", access.ErrNotFound, sessionID)
	}
	return nil
}

// RevokeOthers ends every session of a user but the one given.
func (s *Sessions) RevokeOthers(ctx context.Context, userID, keep string) error {
	if err := s.store.Queries().DeleteOtherSessions(ctx, store.DeleteOtherSessionsParams{UserID: userID, ID: keep}); err != nil {
		return fmt.Errorf("authn: revoking a user's other sessions: %w", err)
	}
	return nil
}

// DeleteForUser ends every session of a user, for a disabled account.
func (s *Sessions) DeleteForUser(ctx context.Context, userID string) error {
	return s.RevokeOthers(ctx, userID, "")
}

type sessionKey struct{}

// WithSession puts the session a call came from in the context, so that
// ListSessions marks it current.
func WithSession(ctx context.Context, sessionID string) context.Context {
	return context.WithValue(ctx, sessionKey{}, sessionID)
}

// SessionFrom reads the session a call came from, empty for a token call.
func SessionFrom(ctx context.Context) string {
	session, _ := ctx.Value(sessionKey{}).(string)
	return session
}
