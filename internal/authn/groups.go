package authn

import (
	"context"
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

// Groups are sets of users, filled by hand or from an OIDC claim, that grants
// name.
type Groups struct {
	store *store.Store
	users *Users
}

// NewGroups returns the groups of a database.
func NewGroups(opened *store.Store) *Groups {
	return &Groups{store: opened, users: NewUsers(opened)}
}

// List is a page of groups with their members and claims.
func (g *Groups) List(ctx context.Context, cursor string, limit int) ([]store.RookeryGroups, error) {
	groups, err := g.store.Queries().ListGroups(ctx, store.ListGroupsParams{ID: cursor, Limit: clampLimit(limit)})
	if err != nil {
		return nil, fmt.Errorf("authn: listing groups: %w", err)
	}
	return groups, nil
}

// ByID returns a group.
func (g *Groups) ByID(ctx context.Context, groupID string) (store.RookeryGroups, error) {
	group, err := g.store.Queries().GetGroup(ctx, groupID)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return store.RookeryGroups{}, fmt.Errorf("%w: no group %s", access.ErrNotFound, groupID)
	case err != nil:
		return store.RookeryGroups{}, fmt.Errorf("authn: reading the group %s: %w", groupID, err)
	}
	return group, nil
}

// Create makes a group.
func (g *Groups) Create(ctx context.Context, name string) (store.RookeryGroups, error) {
	name = strings.TrimSpace(name)
	if name == "" {
		return store.RookeryGroups{}, fmt.Errorf("%w: a group needs a name", access.ErrInvalid)
	}
	group, err := g.store.Queries().CreateGroup(ctx, store.CreateGroupParams{ID: id.New(), Name: name})
	if err != nil {
		if isUniqueViolation(err) {
			return store.RookeryGroups{}, fmt.Errorf("%w: a group called %s is there already", access.ErrConflict, name)
		}
		return store.RookeryGroups{}, fmt.Errorf("authn: creating the group %s: %w", name, err)
	}
	return group, nil
}

// Rename changes a group's name.
func (g *Groups) Rename(ctx context.Context, groupID, name string) (store.RookeryGroups, error) {
	name = strings.TrimSpace(name)
	if name == "" {
		return store.RookeryGroups{}, fmt.Errorf("%w: a group needs a name", access.ErrInvalid)
	}
	if _, err := g.ByID(ctx, groupID); err != nil {
		return store.RookeryGroups{}, err
	}
	group, err := g.store.Queries().RenameGroup(ctx, store.RenameGroupParams{ID: groupID, Name: name})
	if err != nil {
		if isUniqueViolation(err) {
			return store.RookeryGroups{}, fmt.Errorf("%w: a group called %s is there already", access.ErrConflict, name)
		}
		return store.RookeryGroups{}, fmt.Errorf("authn: renaming the group %s: %w", groupID, err)
	}
	return group, nil
}

// Delete removes a group. Its grants go with it, because the grants' principal
// is the group's id and nothing else names it.
func (g *Groups) Delete(ctx context.Context, groupID string) error {
	deleted, err := g.store.Queries().DeleteGroup(ctx, groupID)
	switch {
	case err != nil:
		return fmt.Errorf("authn: deleting the group %s: %w", groupID, err)
	case deleted == 0:
		return fmt.Errorf("%w: no group %s", access.ErrNotFound, groupID)
	}
	return nil
}

// AddMember puts a user in a group by email.
func (g *Groups) AddMember(ctx context.Context, groupID, email string) error {
	if _, err := g.ByID(ctx, groupID); err != nil {
		return err
	}
	user, err := g.users.ByEmail(ctx, email)
	if err != nil {
		return err
	}
	if err := g.store.Queries().AddGroupMemberManual(ctx, store.AddGroupMemberManualParams{
		GroupID: groupID,
		UserID:  user.ID,
	}); err != nil {
		return fmt.Errorf("authn: adding %s to the group %s: %w", email, groupID, err)
	}
	return nil
}

// RemoveMember takes a user out of a group by email.
func (g *Groups) RemoveMember(ctx context.Context, groupID, email string) error {
	user, err := g.users.ByEmail(ctx, email)
	if err != nil {
		return err
	}
	removed, err := g.store.Queries().RemoveGroupMember(ctx, store.RemoveGroupMemberParams{
		GroupID: groupID,
		UserID:  user.ID,
	})
	switch {
	case err != nil:
		return fmt.Errorf("authn: removing %s from the group %s: %w", email, groupID, err)
	case removed == 0:
		return fmt.Errorf("%w: %s is not in the group", access.ErrNotFound, email)
	}
	return nil
}

// BindClaim binds a group to a claim value.
func (g *Groups) BindClaim(ctx context.Context, groupID, claim, value string) error {
	claim, value = strings.TrimSpace(claim), strings.TrimSpace(value)
	if claim == "" || value == "" {
		return fmt.Errorf("%w: a claim binding needs a claim and a value", access.ErrInvalid)
	}
	if _, err := g.ByID(ctx, groupID); err != nil {
		return err
	}
	if err := g.store.Queries().BindGroupClaim(ctx, store.BindGroupClaimParams{
		GroupID: groupID,
		Claim:   claim,
		Value:   value,
	}); err != nil {
		return fmt.Errorf("authn: binding %s=%s to the group %s: %w", claim, value, groupID, err)
	}
	return nil
}

// UnbindClaim takes a claim binding away.
func (g *Groups) UnbindClaim(ctx context.Context, groupID, claim, value string) error {
	if _, err := g.store.Queries().UnbindGroupClaim(ctx, store.UnbindGroupClaimParams{
		GroupID: groupID,
		Claim:   claim,
		Value:   value,
	}); err != nil {
		return fmt.Errorf("authn: unbinding %s=%s from the group %s: %w", claim, value, groupID, err)
	}
	return nil
}

// SyncClaims replaces a user's claim-derived memberships with the groups the
// claims name, leaving the ones added by hand alone. It runs at each sign-in.
func (g *Groups) SyncClaims(ctx context.Context, userID string, claims map[string][]string) error {
	tx, err := g.store.Pool().Begin(ctx)
	if err != nil {
		return fmt.Errorf("authn: beginning claim refresh: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()
	if _, err := tx.Exec(ctx, "SELECT pg_advisory_xact_lock(hashtext($1))", "oidc-groups:"+userID); err != nil {
		return fmt.Errorf("authn: locking claim refresh: %w", err)
	}
	q := g.store.Queries().WithTx(tx)
	if err := q.DeleteClaimMemberships(ctx, userID); err != nil {
		return fmt.Errorf("authn: clearing claim memberships: %w", err)
	}
	for claim, values := range claims {
		if len(values) == 0 {
			continue
		}
		groupIDs, err := q.ListGroupsForClaim(ctx, store.ListGroupsForClaimParams{
			Claim:   claim,
			Column2: values,
		})
		if err != nil {
			return fmt.Errorf("authn: reading the groups of the claim %s: %w", claim, err)
		}
		for _, groupID := range groupIDs {
			if err := q.AddGroupMember(ctx, store.AddGroupMemberParams{
				GroupID: groupID,
				UserID:  userID,
				Source:  "claim",
			}); err != nil {
				return fmt.Errorf("authn: adding %s to the group %s: %w", userID, groupID, err)
			}
		}
	}
	if err := tx.Commit(ctx); err != nil {
		return fmt.Errorf("authn: committing claim refresh: %w", err)
	}
	return nil
}

// Message is a group as the edge API has it, with its members and claims.
func (g *Groups) Message(ctx context.Context, group store.RookeryGroups) (*rookeryv1.Group, error) {
	members, err := g.store.Queries().ListGroupMembers(ctx, group.ID)
	if err != nil {
		return nil, fmt.Errorf("authn: reading the members of %s: %w", group.ID, err)
	}
	claims, err := g.store.Queries().ListGroupClaims(ctx, group.ID)
	if err != nil {
		return nil, fmt.Errorf("authn: reading the claims of %s: %w", group.ID, err)
	}

	out := &rookeryv1.Group{Id: group.ID, Name: group.Name}
	if !group.CreatedAt.IsZero() {
		out.CreatedAt = timestamppb.New(group.CreatedAt)
	}
	for _, member := range members {
		out.Members = append(out.Members, Message(member))
	}
	for _, claim := range claims {
		out.Claims = append(out.Claims, &rookeryv1.GroupClaim{Claim: claim.Claim, Value: claim.Value})
	}
	return out, nil
}

// isUniqueViolation reports a Postgres unique constraint error.
func isUniqueViolation(err error) bool {
	var pgErr interface{ SQLState() string }
	return errors.As(err, &pgErr) && pgErr.SQLState() == "23505"
}
