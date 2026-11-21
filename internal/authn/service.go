package authn

import (
	"context"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/store"
)

// Service is AuthService: the caller's own account.
type Service struct {
	users    *Users
	sessions *Sessions
	tokens   *Tokens
}

// NewService returns AuthService for a database.
func NewService(users *Users, sessions *Sessions, tokens *Tokens) *Service {
	return &Service{users: users, sessions: sessions, tokens: tokens}
}

// Me returns the caller.
func (s *Service) Me(ctx context.Context, _ *connect.Request[rookeryv1.AuthServiceMeRequest]) (*connect.Response[rookeryv1.AuthServiceMeResponse], error) {
	principal := access.MustPrincipal(ctx)
	user, err := s.users.ByID(ctx, principal.ID)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.AuthServiceMeResponse{User: Message(user)}), nil
}

// ListSessions returns the caller's device sessions, the current one marked.
func (s *Service) ListSessions(ctx context.Context, _ *connect.Request[rookeryv1.AuthServiceListSessionsRequest]) (*connect.Response[rookeryv1.AuthServiceListSessionsResponse], error) {
	principal := access.MustPrincipal(ctx)
	sessions, err := s.sessions.List(ctx, principal.ID)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	current := SessionFrom(ctx)
	out := make([]*rookeryv1.Session, 0, len(sessions))
	for _, session := range sessions {
		out = append(out, SessionMessage(session, current))
	}
	return connect.NewResponse(&rookeryv1.AuthServiceListSessionsResponse{Sessions: out}), nil
}

// RevokeSession ends one of the caller's sessions.
func (s *Service) RevokeSession(ctx context.Context, request *connect.Request[rookeryv1.AuthServiceRevokeSessionRequest]) (*connect.Response[rookeryv1.AuthServiceRevokeSessionResponse], error) {
	principal := access.MustPrincipal(ctx)
	if err := s.sessions.Revoke(ctx, principal.ID, request.Msg.GetId()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.AuthServiceRevokeSessionResponse{}), nil
}

// CreateToken makes an access token, and returns its secret once.
func (s *Service) CreateToken(ctx context.Context, request *connect.Request[rookeryv1.AuthServiceCreateTokenRequest]) (*connect.Response[rookeryv1.AuthServiceCreateTokenResponse], error) {
	principal := access.MustPrincipal(ctx)
	secret, token, err := s.tokens.Create(ctx, principal.ID, request.Msg.GetName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.AuthServiceCreateTokenResponse{
		Token:     secret,
		TokenInfo: TokenMessage(token),
	}), nil
}

// ListTokens returns the caller's tokens.
func (s *Service) ListTokens(ctx context.Context, _ *connect.Request[rookeryv1.AuthServiceListTokensRequest]) (*connect.Response[rookeryv1.AuthServiceListTokensResponse], error) {
	principal := access.MustPrincipal(ctx)
	tokens, err := s.tokens.List(ctx, principal.ID)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := make([]*rookeryv1.Token, 0, len(tokens))
	for _, token := range tokens {
		out = append(out, TokenMessage(token))
	}
	return connect.NewResponse(&rookeryv1.AuthServiceListTokensResponse{Tokens: out}), nil
}

// RevokeToken deletes one of the caller's tokens.
func (s *Service) RevokeToken(ctx context.Context, request *connect.Request[rookeryv1.AuthServiceRevokeTokenRequest]) (*connect.Response[rookeryv1.AuthServiceRevokeTokenResponse], error) {
	principal := access.MustPrincipal(ctx)
	if err := s.tokens.Revoke(ctx, principal.ID, request.Msg.GetId()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.AuthServiceRevokeTokenResponse{}), nil
}

// UsersService is UserService: the people of the deployment.
type UsersService struct {
	users *Users
	links *Links
}

// NewUsersService returns UserService for a database.
func NewUsersService(users *Users, links *Links) *UsersService {
	return &UsersService{users: users, links: links}
}

// List returns a page of users.
func (s *UsersService) List(ctx context.Context, request *connect.Request[rookeryv1.UserServiceListRequest]) (*connect.Response[rookeryv1.UserServiceListResponse], error) {
	users, err := s.users.List(ctx, request.Msg.GetCursor(), int(request.Msg.GetLimit()))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.UserServiceListResponse{
		Users:      userMessages(users),
		NextCursor: nextCursor(len(users), int(request.Msg.GetLimit()), lastID(users)),
	}), nil
}

// Search finds users by email or name.
func (s *UsersService) Search(ctx context.Context, request *connect.Request[rookeryv1.UserServiceSearchRequest]) (*connect.Response[rookeryv1.UserServiceSearchResponse], error) {
	if request.Msg.GetQuery() == "" {
		return nil, access.ToConnect(access.ErrInvalid)
	}
	users, err := s.users.Search(ctx, request.Msg.GetQuery(), request.Msg.GetCursor(), int(request.Msg.GetLimit()))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.UserServiceSearchResponse{
		Users:      userMessages(users),
		NextCursor: nextCursor(len(users), int(request.Msg.GetLimit()), lastID(users)),
	}), nil
}

// CreateLink makes a one-time sign-in link for an email.
func (s *UsersService) CreateLink(ctx context.Context, request *connect.Request[rookeryv1.UserServiceCreateLinkRequest]) (*connect.Response[rookeryv1.UserServiceCreateLinkResponse], error) {
	principal := access.MustPrincipal(ctx)
	link, token, err := s.links.Create(ctx, request.Msg.GetEmail(), principal.UserID())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.UserServiceCreateLinkResponse{
		Link: LinkMessage(link, s.links.URL(token)),
	}), nil
}

// SetAdmin gives or takes a user's admin flag.
func (s *UsersService) SetAdmin(ctx context.Context, request *connect.Request[rookeryv1.UserServiceSetAdminRequest]) (*connect.Response[rookeryv1.UserServiceSetAdminResponse], error) {
	user, err := s.users.ByID(ctx, request.Msg.GetUser())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	updated, err := s.users.SetFlags(ctx, user.ID, request.Msg.GetAdmin(), user.Disabled)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.UserServiceSetAdminResponse{User: Message(updated)}), nil
}

// SetDisabled stops or lets a user sign in.
func (s *UsersService) SetDisabled(ctx context.Context, request *connect.Request[rookeryv1.UserServiceSetDisabledRequest]) (*connect.Response[rookeryv1.UserServiceSetDisabledResponse], error) {
	user, err := s.users.ByID(ctx, request.Msg.GetUser())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	updated, err := s.users.SetFlags(ctx, user.ID, user.Admin, request.Msg.GetDisabled())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.UserServiceSetDisabledResponse{User: Message(updated)}), nil
}

// GroupsService is GroupService: groups and their members.
type GroupsService struct {
	groups *Groups
}

// NewGroupsService returns GroupService for a database.
func NewGroupsService(groups *Groups) *GroupsService { return &GroupsService{groups: groups} }

// List returns a page of groups.
func (s *GroupsService) List(ctx context.Context, request *connect.Request[rookeryv1.GroupServiceListRequest]) (*connect.Response[rookeryv1.GroupServiceListResponse], error) {
	groups, err := s.groups.List(ctx, request.Msg.GetCursor(), int(request.Msg.GetLimit()))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := make([]*rookeryv1.Group, 0, len(groups))
	for _, group := range groups {
		message, messageErr := s.groups.Message(ctx, group)
		if messageErr != nil {
			return nil, access.ToConnect(messageErr)
		}
		out = append(out, message)
	}
	return connect.NewResponse(&rookeryv1.GroupServiceListResponse{
		Groups:     out,
		NextCursor: nextCursor(len(groups), int(request.Msg.GetLimit()), lastGroupID(groups)),
	}), nil
}

// Create makes a group.
func (s *GroupsService) Create(ctx context.Context, request *connect.Request[rookeryv1.GroupServiceCreateRequest]) (*connect.Response[rookeryv1.GroupServiceCreateResponse], error) {
	group, err := s.groups.Create(ctx, request.Msg.GetName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	message, err := s.groups.Message(ctx, group)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.GroupServiceCreateResponse{Group: message}), nil
}

// Rename changes a group's name.
func (s *GroupsService) Rename(ctx context.Context, request *connect.Request[rookeryv1.GroupServiceRenameRequest]) (*connect.Response[rookeryv1.GroupServiceRenameResponse], error) {
	group, err := s.groups.Rename(ctx, request.Msg.GetGroup(), request.Msg.GetName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	message, err := s.groups.Message(ctx, group)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.GroupServiceRenameResponse{Group: message}), nil
}

// Delete removes a group.
func (s *GroupsService) Delete(ctx context.Context, request *connect.Request[rookeryv1.GroupServiceDeleteRequest]) (*connect.Response[rookeryv1.GroupServiceDeleteResponse], error) {
	if err := s.groups.Delete(ctx, request.Msg.GetGroup()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.GroupServiceDeleteResponse{}), nil
}

// AddMember puts a user in a group.
func (s *GroupsService) AddMember(ctx context.Context, request *connect.Request[rookeryv1.GroupServiceAddMemberRequest]) (*connect.Response[rookeryv1.GroupServiceAddMemberResponse], error) {
	if err := s.groups.AddMember(ctx, request.Msg.GetGroup(), request.Msg.GetEmail()); err != nil {
		return nil, access.ToConnect(err)
	}
	return s.groupAfter(ctx, request.Msg.GetGroup())
}

// RemoveMember takes a user out of a group.
func (s *GroupsService) RemoveMember(ctx context.Context, request *connect.Request[rookeryv1.GroupServiceRemoveMemberRequest]) (*connect.Response[rookeryv1.GroupServiceRemoveMemberResponse], error) {
	if err := s.groups.RemoveMember(ctx, request.Msg.GetGroup(), request.Msg.GetEmail()); err != nil {
		return nil, access.ToConnect(err)
	}
	group, err := s.groups.ByID(ctx, request.Msg.GetGroup())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	message, err := s.groups.Message(ctx, group)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.GroupServiceRemoveMemberResponse{Group: message}), nil
}

// BindClaim binds a group to a claim value.
func (s *GroupsService) BindClaim(ctx context.Context, request *connect.Request[rookeryv1.GroupServiceBindClaimRequest]) (*connect.Response[rookeryv1.GroupServiceBindClaimResponse], error) {
	if err := s.groups.BindClaim(ctx, request.Msg.GetGroup(), request.Msg.GetClaim(), request.Msg.GetValue()); err != nil {
		return nil, access.ToConnect(err)
	}
	group, err := s.groups.ByID(ctx, request.Msg.GetGroup())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	message, err := s.groups.Message(ctx, group)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.GroupServiceBindClaimResponse{Group: message}), nil
}

// groupAfter answers a call with the group as it is now.
func (s *GroupsService) groupAfter(ctx context.Context, groupID string) (*connect.Response[rookeryv1.GroupServiceAddMemberResponse], error) {
	group, err := s.groups.ByID(ctx, groupID)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	message, err := s.groups.Message(ctx, group)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.GroupServiceAddMemberResponse{Group: message}), nil
}

// userMessages is a page of users as the edge API has them.
func userMessages(users []store.RookeryUsers) []*rookeryv1.User {
	out := make([]*rookeryv1.User, 0, len(users))
	for _, user := range users {
		out = append(out, Message(user))
	}
	return out
}

// nextCursor is the cursor of a page that may have another page after it.
func nextCursor(returned, asked int, last string) string {
	limit := asked
	if limit <= 0 {
		limit = 100
	}
	if returned < limit {
		return ""
	}
	return last
}

func lastID(users []store.RookeryUsers) string {
	if len(users) == 0 {
		return ""
	}
	return users[len(users)-1].ID
}

func lastGroupID(groups []store.RookeryGroups) string {
	if len(groups) == 0 {
		return ""
	}
	return groups[len(groups)-1].ID
}
