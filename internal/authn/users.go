// Package authn is people and how they get in: users, one-time links, device
// sessions with a cookie and a CSRF token, access tokens, OIDC, and the groups
// a claim puts a user in.
//
// A session's secret lives in the browser's cookie and only its SHA-256 is in
// the database, so a copy of the database does not sign anybody in. Tokens are
// the same: rkpat_ followed by 32 random bytes in base32, stored as their
// SHA-256.
package authn

import (
	"context"
	"crypto/sha256"
	"errors"
	"fmt"
	"strings"

	"github.com/jackc/pgx/v5"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/id"
	"github.com/aiviarium/rookery/internal/store"
)

// Users reads and writes people.
type Users struct {
	store *store.Store
}

// NewUsers returns the users of a database.
func NewUsers(opened *store.Store) *Users { return &Users{store: opened} }

// Ensure returns the user with an email, making them when they are new. An
// admin listed in ROOKERY_ADMINS starts with the admin flag; anybody else's
// first user is not an admin.
func (u *Users) Ensure(ctx context.Context, email, name string, admin bool) (store.RookeryUsers, error) {
	email = normalizeEmail(email)
	if err := checkEmail(email); err != nil {
		return store.RookeryUsers{}, err
	}

	user, err := u.store.Queries().GetUserByEmail(ctx, email)
	switch {
	case err == nil:
		return user, nil
	case !errors.Is(err, pgx.ErrNoRows):
		return store.RookeryUsers{}, fmt.Errorf("authn: looking up %s: %w", email, err)
	}

	user, err = u.store.Queries().CreateUser(ctx, store.CreateUserParams{
		ID:    id.New(),
		Email: email,
		Name:  strings.TrimSpace(name),
		Admin: admin,
	})
	if err != nil {
		// Two sign-ins of a new person at once: the other one made them.
		user, lookupErr := u.store.Queries().GetUserByEmail(ctx, email)
		if lookupErr == nil {
			return user, nil
		}
		return store.RookeryUsers{}, fmt.Errorf("authn: creating %s: %w", email, err)
	}
	return user, nil
}

// ByID returns a user, or access.ErrNotFound.
func (u *Users) ByID(ctx context.Context, userID string) (store.RookeryUsers, error) {
	user, err := u.store.Queries().GetUser(ctx, userID)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return store.RookeryUsers{}, fmt.Errorf("%w: no user %s", access.ErrNotFound, userID)
	case err != nil:
		return store.RookeryUsers{}, fmt.Errorf("authn: reading the user %s: %w", userID, err)
	}
	return user, nil
}

// ByEmail returns a user, or access.ErrNotFound.
func (u *Users) ByEmail(ctx context.Context, email string) (store.RookeryUsers, error) {
	user, err := u.store.Queries().GetUserByEmail(ctx, normalizeEmail(email))
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return store.RookeryUsers{}, fmt.Errorf("%w: no user %s", access.ErrNotFound, normalizeEmail(email))
	case err != nil:
		return store.RookeryUsers{}, fmt.Errorf("authn: reading the user %s: %w", email, err)
	}
	return user, nil
}

// List is a page of users, by id.
func (u *Users) List(ctx context.Context, cursor string, limit int) ([]store.RookeryUsers, error) {
	users, err := u.store.Queries().ListUsers(ctx, store.ListUsersParams{ID: cursor, Limit: clampLimit(limit)})
	if err != nil {
		return nil, fmt.Errorf("authn: listing users: %w", err)
	}
	return users, nil
}

// Search finds users by email or name.
func (u *Users) Search(ctx context.Context, query, cursor string, limit int) ([]store.RookeryUsers, error) {
	users, err := u.store.Queries().SearchUsers(ctx, store.SearchUsersParams{
		Email: likePattern(query),
		ID:    cursor,
		Limit: clampLimit(limit),
	})
	if err != nil {
		return nil, fmt.Errorf("authn: searching users: %w", err)
	}
	return users, nil
}

// SetFlags sets a user's admin and disabled flags. It refuses to leave the
// deployment without an admin who can sign in, and a caller may not disable
// itself.
func (u *Users) SetFlags(ctx context.Context, userID string, admin, disabled bool) (store.RookeryUsers, error) {
	user, err := u.ByID(ctx, userID)
	if err != nil {
		return store.RookeryUsers{}, err
	}
	if caller, ok := access.PrincipalFrom(ctx); ok && caller.ID == userID && disabled {
		return store.RookeryUsers{}, fmt.Errorf("%w: you cannot disable yourself", access.ErrInvalid)
	}

	if user.Admin && (!admin || disabled) {
		admins, countErr := u.store.Queries().CountAdmins(ctx)
		if countErr != nil {
			return store.RookeryUsers{}, fmt.Errorf("authn: counting admins: %w", countErr)
		}
		if admins <= 1 {
			return store.RookeryUsers{}, fmt.Errorf("%w: the deployment would have no admin left", access.ErrInvalid)
		}
	}

	updated, err := u.store.Queries().SetUserFlags(ctx, store.SetUserFlagsParams{
		ID:       userID,
		Admin:    admin,
		Disabled: disabled,
	})
	if err != nil {
		return store.RookeryUsers{}, fmt.Errorf("authn: setting the flags of %s: %w", userID, err)
	}
	return updated, nil
}

// Message is a user as the edge API has it.
func Message(user store.RookeryUsers) *rookeryv1.User {
	out := &rookeryv1.User{
		Id:       user.ID,
		Email:    user.Email,
		Name:     user.Name,
		Admin:    user.Admin,
		Disabled: user.Disabled,
	}
	if !user.CreatedAt.IsZero() {
		out.CreatedAt = timestamppb.New(user.CreatedAt)
	}
	return out
}

// Principal is a user as a caller.
func Principal(user store.RookeryUsers) access.Principal {
	return access.Principal{
		Kind:  access.KindUser,
		ID:    user.ID,
		Email: user.Email,
		Name:  user.Name,
		Admin: user.Admin,
	}
}

// normalizeEmail is how an email is stored and looked up: lower case, without
// spaces around it.
func normalizeEmail(email string) string {
	return strings.ToLower(strings.TrimSpace(email))
}

// checkEmail refuses something that is not an email address at all.
func checkEmail(email string) error {
	local, domain, found := strings.Cut(email, "@")
	if !found || local == "" || domain == "" || !strings.Contains(domain, ".") {
		return fmt.Errorf("%w: %q is not an email address", access.ErrInvalid, email)
	}
	return nil
}

// likePattern is an ILIKE pattern for a search: the query with its wildcards
// escaped, between two wildcards.
func likePattern(query string) string {
	escaped := strings.NewReplacer(`\`, `\\`, `%`, `\%`, `_`, `\_`).Replace(strings.TrimSpace(query))
	return "%" + escaped + "%"
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

// hash is the SHA-256 a secret is stored as.
func hash(secret string) []byte {
	sum := sha256.Sum256([]byte(secret))
	return sum[:]
}
