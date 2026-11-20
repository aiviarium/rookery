package access

import (
	"context"
	"fmt"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/store"
)

// Service is AccessService: it reads and changes grants.
type Service struct {
	checker *Checker
	store   *store.Store
}

// NewService returns AccessService for a database.
func NewService(checker *Checker, opened *store.Store) *Service {
	return &Service{checker: checker, store: opened}
}

// Check says whether the caller holds a level on a resource.
func (s *Service) Check(ctx context.Context, request *connect.Request[rookeryv1.AccessServiceCheckRequest]) (*connect.Response[rookeryv1.AccessServiceCheckResponse], error) {
	principal := MustPrincipal(ctx)
	held, err := s.checker.Level(ctx, principal, request.Msg.GetResource())
	if err != nil {
		return nil, ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.AccessServiceCheckResponse{
		Allowed: Satisfies(held, request.Msg.GetLevel()),
		Level:   held,
	}), nil
}

// Level returns the highest level the caller holds on a resource.
func (s *Service) Level(ctx context.Context, request *connect.Request[rookeryv1.AccessServiceLevelRequest]) (*connect.Response[rookeryv1.AccessServiceLevelResponse], error) {
	principal := MustPrincipal(ctx)
	held, err := s.checker.Level(ctx, principal, request.Msg.GetResource())
	if err != nil {
		return nil, ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.AccessServiceLevelResponse{Level: held}), nil
}

// ListGrants lists a resource's grants, or every grant when the request names no
// resource, which only an admin may do.
func (s *Service) ListGrants(ctx context.Context, request *connect.Request[rookeryv1.AccessServiceListGrantsRequest]) (*connect.Response[rookeryv1.AccessServiceListGrantsResponse], error) {
	resource := request.Msg.GetResource()
	if resource == "" {
		principal := MustPrincipal(ctx)
		if !principal.IsAdmin() {
			return nil, ToConnect(&Denied{Need: Admin, Reason: "every grant is an admin's to read"})
		}
		return s.allGrants(ctx, request.Msg.GetCursor(), int(request.Msg.GetLimit()))
	}

	if _, err := ParseResource(resource); err != nil {
		return nil, ToConnect(err)
	}
	grants, err := s.store.Queries().ListGrantsForResource(ctx, resource)
	if err != nil {
		return nil, ToConnect(fmt.Errorf("access: listing the grants of %s: %w", resource, err))
	}
	out := make([]*rookeryv1.Grant, 0, len(grants))
	for _, grant := range grants {
		out = append(out, GrantMessage(grant))
	}
	return connect.NewResponse(&rookeryv1.AccessServiceListGrantsResponse{Grants: out}), nil
}

// allGrants pages every grant, by resource and then principal.
func (s *Service) allGrants(ctx context.Context, cursor string, limit int) (*connect.Response[rookeryv1.AccessServiceListGrantsResponse], error) {
	afterResource, afterPrincipal := "", ""
	if cursor != "" {
		resource, principal, found := cutCursor(cursor)
		if !found {
			return nil, ToConnect(fmt.Errorf("%w: %q is not a cursor", ErrInvalid, cursor))
		}
		afterResource, afterPrincipal = resource, principal
	}
	grants, err := s.store.Queries().ListGrants(ctx, store.ListGrantsParams{
		AfterResource:  afterResource,
		AfterPrincipal: afterPrincipal,
		RowLimit:       clampLimit(limit),
	})
	if err != nil {
		return nil, ToConnect(fmt.Errorf("access: listing grants: %w", err))
	}
	out := make([]*rookeryv1.Grant, 0, len(grants))
	for _, grant := range grants {
		out = append(out, GrantMessage(grant))
	}
	next := ""
	rowLimit := int(clampLimit(limit))
	if len(grants) == rowLimit {
		last := grants[len(grants)-1]
		next = cursorOf(last.Resource, last.Principal)
	}
	return connect.NewResponse(&rookeryv1.AccessServiceListGrantsResponse{Grants: out, NextCursor: next}), nil
}

// Grant gives a principal a level on a resource.
func (s *Service) Grant(ctx context.Context, request *connect.Request[rookeryv1.AccessServiceGrantRequest]) (*connect.Response[rookeryv1.AccessServiceGrantResponse], error) {
	principal, err := principalOf(request.Msg.GetGrantee())
	if err != nil {
		return nil, ToConnect(err)
	}
	grant, err := s.checker.Grant(ctx, request.Msg.GetResource(), principal, request.Msg.GetLevel())
	if err != nil {
		return nil, ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.AccessServiceGrantResponse{Grant: GrantMessage(grant)}), nil
}

// Revoke takes a principal's grant on a resource away.
func (s *Service) Revoke(ctx context.Context, request *connect.Request[rookeryv1.AccessServiceRevokeRequest]) (*connect.Response[rookeryv1.AccessServiceRevokeResponse], error) {
	principal, err := principalOf(request.Msg.GetGrantee())
	if err != nil {
		return nil, ToConnect(err)
	}
	if err := s.checker.Revoke(ctx, request.Msg.GetResource(), principal); err != nil {
		return nil, ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.AccessServiceRevokeResponse{}), nil
}

// principalOf is a grant request's grantee as a grant principal.
func principalOf(grantee *rookeryv1.Grantee) (string, error) {
	switch who := grantee.GetWho().(type) {
	case *rookeryv1.Grantee_UserId:
		if who.UserId == "" {
			return "", fmt.Errorf("%w: a grantee needs a user", ErrInvalid)
		}
		return UserPrincipal(who.UserId), nil
	case *rookeryv1.Grantee_GroupId:
		if who.GroupId == "" {
			return "", fmt.Errorf("%w: a grantee needs a group", ErrInvalid)
		}
		return GroupPrincipal(who.GroupId), nil
	case *rookeryv1.Grantee_Everyone:
		if !who.Everyone {
			return "", fmt.Errorf("%w: a grantee is a user, a group or everyone", ErrInvalid)
		}
		return Everyone, nil
	default:
		return "", fmt.Errorf("%w: a grantee is a user, a group or everyone", ErrInvalid)
	}
}

// GrantMessage is a grant as the edge API has it.
func GrantMessage(grant store.RookeryGrants) *rookeryv1.Grant {
	out := &rookeryv1.Grant{
		Resource: grant.Resource,
		Level:    Level(grant.Level),
		Grantee:  granteeOf(grant.Principal),
	}
	if !grant.CreatedAt.IsZero() {
		out.CreatedAt = timestamppb.New(grant.CreatedAt)
	}
	return out
}

// granteeOf reads a grant principal back into the API's one-of.
func granteeOf(principal string) *rookeryv1.Grantee {
	if principal == Everyone {
		return &rookeryv1.Grantee{Who: &rookeryv1.Grantee_Everyone{Everyone: true}}
	}
	kind, id, _ := cutPrincipal(principal)
	switch kind {
	case KindUser:
		return &rookeryv1.Grantee{Who: &rookeryv1.Grantee_UserId{UserId: id}}
	case "group":
		return &rookeryv1.Grantee{Who: &rookeryv1.Grantee_GroupId{GroupId: id}}
	default:
		return &rookeryv1.Grantee{}
	}
}

// cursorOf joins a grant's place in the list into a cursor.
func cursorOf(resource, principal string) string {
	return resource + "\x00" + principal
}

// cutCursor splits a grant cursor.
func cutCursor(cursor string) (resource, principal string, found bool) {
	for i := 0; i < len(cursor); i++ {
		if cursor[i] == 0 {
			return cursor[:i], cursor[i+1:], true
		}
	}
	return "", "", false
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
