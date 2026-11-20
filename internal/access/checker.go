package access

import (
	"context"
	"errors"
	"fmt"
	"strings"

	"github.com/jackc/pgx/v5"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/store"
)

// Checker answers what a principal holds on a resource, and enforces it.
//
// It reads the grant chain in one query: the resource, its parents, and the
// highest level granted on any of them to the caller, the caller's groups or
// everyone. Admins and the local admin hold manage everywhere without a query.
type Checker struct {
	store *store.Store
}

// NewChecker returns the checker for a database.
func NewChecker(opened *store.Store) *Checker {
	return &Checker{store: opened}
}

// Level is the highest level principal holds on resource. An empty resource is
// the deployment itself: nobody holds a level on it, and only an admin or the
// local admin reaches a manage requirement there.
func (c *Checker) Level(ctx context.Context, principal Principal, resource string) (Level, error) {
	switch {
	case !principal.Authenticated():
		return rookeryv1.Level_LEVEL_UNSPECIFIED, ErrUnauthenticated
	case principal.IsAdmin():
		return Manage, nil
	case resource == "":
		return rookeryv1.Level_LEVEL_UNSPECIFIED, nil
	case principal.Kind == KindUser && resource == KindUser+"://"+principal.ID:
		return Manage, nil
	}

	kind, err := ParseResource(resource)
	if err != nil {
		return rookeryv1.Level_LEVEL_UNSPECIFIED, err
	}
	if kind == "blob" {
		blob, err := c.store.Queries().GetBlob(ctx, strings.TrimPrefix(resource, "blob://"))
		if errors.Is(err, pgx.ErrNoRows) {
			return rookeryv1.Level_LEVEL_UNSPECIFIED, nil
		}
		if err != nil {
			return rookeryv1.Level_LEVEL_UNSPECIFIED, fmt.Errorf("access: reading %s: %w", resource, err)
		}
		if blob.Owner == principal.String() {
			return Manage, nil
		}
		if blob.Resource == "" {
			return rookeryv1.Level_LEVEL_UNSPECIFIED, nil
		}
		resource = blob.Resource
	}

	groups, err := c.groupsOf(ctx, principal)
	if err != nil {
		return rookeryv1.Level_LEVEL_UNSPECIFIED, err
	}

	held, err := c.store.Queries().HeldLevel(ctx, store.HeldLevelParams{
		Uri:     resource,
		Column2: principal.grantPrincipals(groups),
	})
	if err != nil {
		return rookeryv1.Level_LEVEL_UNSPECIFIED, fmt.Errorf("access: reading the level on %s: %w", resource, err)
	}
	if held > int32(Manage) {
		// A row written by another version of Rookery: manage is the top.
		held = int32(Manage)
	}
	return Level(held), nil
}

// Check refuses unless principal holds need on resource. The refusal is a
// *Denied, which says what was missing.
func (c *Checker) Check(ctx context.Context, principal Principal, resource string, need Level) error {
	if err := c.checkPrincipal(principal, need); err != nil {
		return err
	}
	if resource == "" {
		// Nothing is granted on the deployment itself: view and use are every
		// signed-in caller's, and manage is an admin's.
		if need == Manage && !principal.IsAdmin() {
			return &Denied{Need: need, Reason: "the admin flag"}
		}
		return nil
	}
	held, err := c.Level(ctx, principal, resource)
	if err != nil {
		return err
	}
	if !Satisfies(held, need) {
		return &Denied{Resource: resource, Need: need, Reason: "been given " + LevelName(need)}
	}
	return nil
}

// checkPrincipal answers the requirements that are about the caller rather than
// a resource.
func (c *Checker) checkPrincipal(principal Principal, need Level) error {
	if !principal.Authenticated() {
		return ErrUnauthenticated
	}
	switch need {
	case Local:
		if !principal.IsLocal() {
			return &Denied{Need: need, Reason: "called on the server's Unix socket"}
		}
	case Admin:
		if !principal.IsAdmin() {
			return &Denied{Need: need, Reason: "the admin flag"}
		}
	case Self:
		// Any signed-in principal may act on itself.
	}
	return nil
}

// groupsOf is the ids of the groups a principal is in.
func (c *Checker) groupsOf(ctx context.Context, principal Principal) ([]string, error) {
	if principal.Kind != KindUser {
		return nil, nil
	}
	groups, err := c.store.Queries().ListGroupsForUser(ctx, principal.ID)
	if err != nil {
		return nil, fmt.Errorf("access: reading the groups of %s: %w", principal, err)
	}
	ids := make([]string, 0, len(groups))
	for _, group := range groups {
		ids = append(ids, group.ID)
	}
	return ids, nil
}

// Grant gives a principal a level on a resource, creating the resource's row
// when it is not there. The parent is the resource's own, or empty.
func (c *Checker) Grant(ctx context.Context, resource, principal string, level Level) (store.RookeryGrants, error) {
	if _, err := ParseResource(resource); err != nil {
		return store.RookeryGrants{}, err
	}
	if !grantableLevel(level) {
		return store.RookeryGrants{}, fmt.Errorf("%w: %s is not a level a grant carries: view, use or manage", ErrInvalid, LevelName(level))
	}
	if err := checkGrantPrincipal(principal); err != nil {
		return store.RookeryGrants{}, err
	}

	tx, err := c.store.Pool().Begin(ctx)
	if err != nil {
		return store.RookeryGrants{}, fmt.Errorf("access: beginning the grant: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	queries := c.store.Queries().WithTx(tx)
	if err := queries.UpsertResource(ctx, store.UpsertResourceParams{Uri: resource}); err != nil {
		return store.RookeryGrants{}, fmt.Errorf("access: recording the resource %s: %w", resource, err)
	}
	grant, err := queries.UpsertGrant(ctx, store.UpsertGrantParams{
		Resource:  resource,
		Principal: principal,
		Level:     int16(min(max(level, View), Manage)),
	})
	if err != nil {
		return store.RookeryGrants{}, fmt.Errorf("access: granting %s on %s: %w", LevelName(level), resource, err)
	}
	if err := tx.Commit(ctx); err != nil {
		return store.RookeryGrants{}, fmt.Errorf("access: committing the grant: %w", err)
	}
	return grant, nil
}

// Revoke takes a principal's grant on a resource away. A principal with no
// grant there is not an error: revoking twice is what a script does.
func (c *Checker) Revoke(ctx context.Context, resource, principal string) error {
	if _, err := ParseResource(resource); err != nil {
		return err
	}
	if err := checkGrantPrincipal(principal); err != nil {
		return err
	}
	if _, err := c.store.Queries().DeleteGrant(ctx, store.DeleteGrantParams{
		Resource:  resource,
		Principal: principal,
	}); err != nil {
		return fmt.Errorf("access: revoking %s on %s: %w", principal, resource, err)
	}
	return nil
}

// GrantUser gives a user a level on a resource. Creating a space, a host or a
// document grants its creator manage, and calls this.
func (c *Checker) GrantUser(ctx context.Context, userID, resource string, level Level) error {
	if userID == "" {
		return nil
	}
	_, err := c.Grant(ctx, resource, UserPrincipal(userID), level)
	return err
}

// SetParent records a resource's parent, which is how a grant on the parent
// reaches it.
func (c *Checker) SetParent(ctx context.Context, resource, parent string) error {
	if _, err := ParseResource(resource); err != nil {
		return err
	}
	if _, err := ParseResource(parent); err != nil {
		return err
	}
	err := c.store.Queries().UpsertResource(ctx, store.UpsertResourceParams{Uri: resource, Parent: &parent})
	if err != nil {
		return fmt.Errorf("access: setting the parent of %s: %w", resource, err)
	}
	return nil
}

// ForResource returns the resource a call names, creating it when it is not
// there and its kind is not one another package owns the row of.
func (c *Checker) ForResource(ctx context.Context, resource string) error {
	if _, err := ParseResource(resource); err != nil {
		return err
	}
	return c.store.Queries().UpsertResource(ctx, store.UpsertResourceParams{Uri: resource})
}

// grantableLevel reports whether a grant can carry the level: only the chain's
// three.
func grantableLevel(level Level) bool {
	return level == View || level == Use || level == Manage
}

// checkGrantPrincipal validates a grant's principal string.
func checkGrantPrincipal(principal string) error {
	if principal == Everyone {
		return nil
	}
	kind, id, found := cutPrincipal(principal)
	if !found || id == "" || (kind != KindUser && kind != "group") {
		return fmt.Errorf("%w: %q is not a principal: user:ID, group:ID or everyone", ErrInvalid, principal)
	}
	return nil
}

// cutPrincipal splits "user:01h…" into its kind and id.
func cutPrincipal(principal string) (kind, id string, found bool) {
	for i := 0; i < len(principal); i++ {
		if principal[i] == ':' {
			return principal[:i], principal[i+1:], true
		}
	}
	return "", "", false
}

// IsMissing reports whether err is a row that is not there.
func IsMissing(err error) bool { return errors.Is(err, pgx.ErrNoRows) }
