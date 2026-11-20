package authn

import (
	"context"
	"errors"
	"fmt"
	"net/url"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/id"
	"github.com/aiviarium/rookery/internal/store"
)

// LinkLifetime is how long a sign-in link works for.
const LinkLifetime = 24 * time.Hour

// linkSecretBytes is how much randomness a link's token holds.
const linkSecretBytes = 24

// Links are one-time sign-in links.
type Links struct {
	store   *store.Store
	baseURL string
}

// NewLinks returns the sign-in links of a deployment. baseURL is the public
// address links are made under, such as https://rookery.example.com; empty
// leaves the URL to the edge, which knows the address of the request.
func NewLinks(opened *store.Store, baseURL string) *Links {
	return &Links{store: opened, baseURL: strings.TrimRight(baseURL, "/")}
}

// Create makes a link for an email. An empty email makes the setup link, which
// the first admin signs in with: it has no user or email yet, and the person
// using it names themselves.
func (l *Links) Create(ctx context.Context, email, createdBy string) (store.RookeryLinks, string, error) {
	email = normalizeEmail(email)
	if email != "" {
		if err := checkEmail(email); err != nil {
			return store.RookeryLinks{}, "", err
		}
	}

	token := id.Secret(linkSecretBytes)
	params := store.CreateLinkParams{
		TokenHash: hash(token),
		Email:     email,
		ExpiresAt: time.Now().Add(LinkLifetime),
	}
	if createdBy != "" {
		params.CreatedBy = &createdBy
	}
	// A link for an email that already has a user signs that user in; one for a
	// new email makes the user on first use.
	if email != "" {
		if user, err := l.store.Queries().GetUserByEmail(ctx, email); err == nil {
			params.UserID = &user.ID
		} else if !errors.Is(err, pgx.ErrNoRows) {
			return store.RookeryLinks{}, "", fmt.Errorf("authn: looking up %s: %w", email, err)
		}
	}

	link, err := l.store.Queries().CreateLink(ctx, params)
	if err != nil {
		return store.RookeryLinks{}, "", fmt.Errorf("authn: creating a link: %w", err)
	}
	return link, token, nil
}

// URL is the address a link's token is at. An empty baseURL leaves the token
// alone, as "/auth/link/TOKEN", for a caller that knows the address.
func (l *Links) URL(token string) string {
	return l.baseURL + "/auth/link/" + url.PathEscape(token)
}

// Check returns a link without using it, or access.ErrNotFound when it is
// unknown, spent or expired.
func (l *Links) Check(ctx context.Context, token string) (store.RookeryLinks, error) {
	link, err := l.store.Queries().GetLink(ctx, hash(token))
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return store.RookeryLinks{}, fmt.Errorf("%w: the link is not known", access.ErrNotFound)
	case err != nil:
		return store.RookeryLinks{}, fmt.Errorf("authn: reading a link: %w", err)
	case link.UsedAt != nil:
		return store.RookeryLinks{}, fmt.Errorf("%w: the link was already used", access.ErrNotFound)
	case !link.ExpiresAt.After(time.Now()):
		return store.RookeryLinks{}, fmt.Errorf("%w: the link has expired", access.ErrNotFound)
	}
	return link, nil
}

// Consume spends a link: it works once. A second call with the same token finds
// nothing.
func (l *Links) Consume(ctx context.Context, token string) (store.RookeryLinks, error) {
	link, err := l.Check(ctx, token)
	if err != nil {
		return store.RookeryLinks{}, err
	}
	used, err := l.store.Queries().UseLink(ctx, hash(token))
	switch {
	case err != nil:
		return store.RookeryLinks{}, fmt.Errorf("authn: using a link: %w", err)
	case used == 0:
		// Another call spent it between the check and the update.
		return store.RookeryLinks{}, fmt.Errorf("%w: the link was already used", access.ErrNotFound)
	}
	return link, nil
}
