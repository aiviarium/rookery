package authn_test

import (
	"context"
	"net/http"
	"strings"
	"testing"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/access/accesstest"
	"github.com/aiviarium/rookery/internal/authn"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// clients are the three services a caller signs in with.
type clients struct {
	server *accesstest.Server
	auth   rookeryv1connect.AuthServiceClient
	users  rookeryv1connect.UserServiceClient
	groups rookeryv1connect.GroupServiceClient
}

// start serves AuthService, UserService and GroupService for a caller.
func start(t *testing.T, opened *store.Store, principal access.Principal) clients {
	t.Helper()

	users := authn.NewUsers(opened)
	sessions := authn.NewSessions(opened)
	tokens := authn.NewTokens(opened)
	groups := authn.NewGroups(opened)
	links := authn.NewLinks(opened, "https://rookery.example.test")

	authService := authn.NewService(users, sessions, tokens)
	userService := authn.NewUsersService(users, links)
	groupService := authn.NewGroupsService(groups)

	checker := access.NewChecker(opened)
	server := accesstest.New(t, checker, principal, func(mux *http.ServeMux, options []connect.HandlerOption) {
		path, handler := rookeryv1connect.NewAuthServiceHandler(authService, options...)
		mux.Handle(path, handler)
		path, handler = rookeryv1connect.NewUserServiceHandler(userService, options...)
		mux.Handle(path, handler)
		path, handler = rookeryv1connect.NewGroupServiceHandler(groupService, options...)
		mux.Handle(path, handler)
	})

	return clients{
		server: server,
		auth:   rookeryv1connect.NewAuthServiceClient(server.Client(), server.URL, connect.WithGRPC()),
		users:  rookeryv1connect.NewUserServiceClient(server.Client(), server.URL, connect.WithGRPC()),
		groups: rookeryv1connect.NewGroupServiceClient(server.Client(), server.URL, connect.WithGRPC()),
	}
}

func TestMeAndSessions(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	ctx := t.Context()
	user := makeUser(t, opened, "alice", false)
	sessions := authn.NewSessions(opened)
	_, current, err := sessions.Create(ctx, user, "this browser")
	if err != nil {
		t.Fatalf("creating a session: %v", err)
	}
	if _, _, err := sessions.Create(ctx, user, "another browser"); err != nil {
		t.Fatalf("creating a session: %v", err)
	}

	// The edge's middleware puts the session the call came from in the context.
	caller := start(t, opened, accesstest.User(user, false))
	caller.server.Decorate = func(ctx context.Context) context.Context {
		return authn.WithSession(ctx, current.ID)
	}

	me, err := caller.auth.Me(ctx, connect.NewRequest(&rookeryv1.AuthServiceMeRequest{}))
	if err != nil {
		t.Fatalf("Me: %v", err)
	}
	if me.Msg.GetUser().GetEmail() != "alice@example.test" {
		t.Errorf("Me answered with %q", me.Msg.GetUser().GetEmail())
	}

	list, err := caller.auth.ListSessions(ctx, connect.NewRequest(&rookeryv1.AuthServiceListSessionsRequest{}))
	if err != nil {
		t.Fatalf("ListSessions: %v", err)
	}
	if len(list.Msg.GetSessions()) != 2 {
		t.Fatalf("ListSessions returned %d sessions, want 2", len(list.Msg.GetSessions()))
	}
	currentMarked := 0
	var otherID string
	for _, session := range list.Msg.GetSessions() {
		if session.GetCurrent() {
			currentMarked++
			if session.GetId() != current.ID {
				t.Errorf("the wrong session is marked current: %s", session.GetId())
			}
		} else {
			otherID = session.GetId()
		}
	}
	if currentMarked != 1 {
		t.Fatalf("%d sessions are marked current, want 1", currentMarked)
	}

	if _, err := caller.auth.RevokeSession(ctx, connect.NewRequest(&rookeryv1.AuthServiceRevokeSessionRequest{Id: otherID})); err != nil {
		t.Fatalf("RevokeSession: %v", err)
	}
	list, err = caller.auth.ListSessions(ctx, connect.NewRequest(&rookeryv1.AuthServiceListSessionsRequest{}))
	if err != nil {
		t.Fatalf("ListSessions: %v", err)
	}
	if len(list.Msg.GetSessions()) != 1 {
		t.Fatalf("a revoked session is still listed: %d sessions", len(list.Msg.GetSessions()))
	}
}

func TestTokensThroughTheService(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	ctx := t.Context()
	user := makeUser(t, opened, "alice", false)
	caller := start(t, opened, accesstest.User(user, false))

	created, err := caller.auth.CreateToken(ctx, connect.NewRequest(&rookeryv1.AuthServiceCreateTokenRequest{Name: "laptop"}))
	if err != nil {
		t.Fatalf("CreateToken: %v", err)
	}
	if !strings.HasPrefix(created.Msg.GetToken(), authn.TokenPrefix) {
		t.Errorf("the token %q is not an access token", created.Msg.GetToken())
	}

	list, err := caller.auth.ListTokens(ctx, connect.NewRequest(&rookeryv1.AuthServiceListTokensRequest{}))
	if err != nil {
		t.Fatalf("ListTokens: %v", err)
	}
	if len(list.Msg.GetTokens()) != 1 || list.Msg.GetTokens()[0].GetName() != "laptop" {
		t.Fatalf("ListTokens returned %+v", list.Msg.GetTokens())
	}

	if _, err := caller.auth.RevokeToken(ctx, connect.NewRequest(&rookeryv1.AuthServiceRevokeTokenRequest{Id: created.Msg.GetTokenInfo().GetId()})); err != nil {
		t.Fatalf("RevokeToken: %v", err)
	}
	list, err = caller.auth.ListTokens(ctx, connect.NewRequest(&rookeryv1.AuthServiceListTokensRequest{}))
	if err != nil {
		t.Fatalf("ListTokens: %v", err)
	}
	if len(list.Msg.GetTokens()) != 0 {
		t.Fatalf("a revoked token is still listed: %d", len(list.Msg.GetTokens()))
	}
}

func TestOnlyAnAdminMakesLinks(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	ctx := t.Context()
	alice := makeUser(t, opened, "alice", false)
	admin := makeUser(t, opened, "admin", true)

	asAlice := start(t, opened, accesstest.User(alice, false))
	if _, err := asAlice.users.CreateLink(ctx, connect.NewRequest(&rookeryv1.UserServiceCreateLinkRequest{Email: "bob@example.test"})); err == nil {
		t.Fatal("a user who is not an admin made a link")
	} else if connect.CodeOf(err) != connect.CodePermissionDenied {
		t.Fatalf("a user who is not an admin failed with %v, want permission denied", err)
	}

	asAdmin := start(t, opened, accesstest.User(admin, true))
	created, err := asAdmin.users.CreateLink(ctx, connect.NewRequest(&rookeryv1.UserServiceCreateLinkRequest{Email: "bob@example.test"}))
	if err != nil {
		t.Fatalf("an admin made a link: %v", err)
	}
	if !strings.HasPrefix(created.Msg.GetLink().GetUrl(), "https://rookery.example.test/auth/link/") {
		t.Errorf("the link is %q", created.Msg.GetLink().GetUrl())
	}
	if created.Msg.GetLink().GetExpiresAt() == nil {
		t.Error("the link says no expiry")
	}

	// A non-admin may still list people: the directory is everyone's.
	if _, err := asAlice.users.List(ctx, connect.NewRequest(&rookeryv1.UserServiceListRequest{})); err != nil {
		t.Fatalf("a user listed people: %v", err)
	}
}

func TestGroupsThroughTheService(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	ctx := t.Context()
	admin := makeUser(t, opened, "admin", true)
	alice := makeUser(t, opened, "alice", false)

	asAlice := start(t, opened, accesstest.User(alice, false))
	if _, err := asAlice.groups.Create(ctx, connect.NewRequest(&rookeryv1.GroupServiceCreateRequest{Name: "writers"})); err == nil {
		t.Fatal("a user who is not an admin made a group")
	}

	asAdmin := start(t, opened, accesstest.User(admin, true))
	created, err := asAdmin.groups.Create(ctx, connect.NewRequest(&rookeryv1.GroupServiceCreateRequest{Name: "writers"}))
	if err != nil {
		t.Fatalf("creating a group: %v", err)
	}
	groupID := created.Msg.GetGroup().GetId()

	added, err := asAdmin.groups.AddMember(ctx, connect.NewRequest(&rookeryv1.GroupServiceAddMemberRequest{
		Group: groupID,
		Email: "alice@example.test",
	}))
	if err != nil {
		t.Fatalf("adding a member: %v", err)
	}
	if len(added.Msg.GetGroup().GetMembers()) != 1 {
		t.Fatalf("the group has %d members, want 1", len(added.Msg.GetGroup().GetMembers()))
	}

	bound, err := asAdmin.groups.BindClaim(ctx, connect.NewRequest(&rookeryv1.GroupServiceBindClaimRequest{
		Group: groupID,
		Claim: "groups",
		Value: "writers",
	}))
	if err != nil {
		t.Fatalf("binding a claim: %v", err)
	}
	if len(bound.Msg.GetGroup().GetClaims()) != 1 {
		t.Fatalf("the group has %d claims, want 1", len(bound.Msg.GetGroup().GetClaims()))
	}

	removed, err := asAdmin.groups.RemoveMember(ctx, connect.NewRequest(&rookeryv1.GroupServiceRemoveMemberRequest{
		Group: groupID,
		Email: "alice@example.test",
	}))
	if err != nil {
		t.Fatalf("removing a member: %v", err)
	}
	if len(removed.Msg.GetGroup().GetMembers()) != 0 {
		t.Fatalf("the group still has %d members", len(removed.Msg.GetGroup().GetMembers()))
	}

	if _, err := asAdmin.groups.Delete(ctx, connect.NewRequest(&rookeryv1.GroupServiceDeleteRequest{Group: groupID})); err != nil {
		t.Fatalf("deleting a group: %v", err)
	}
}

func TestAnAnonymousCallerIsRefused(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	caller := start(t, opened, accesstest.Anonymous())

	if _, err := caller.auth.Me(t.Context(), connect.NewRequest(&rookeryv1.AuthServiceMeRequest{})); err == nil {
		t.Fatal("an anonymous caller asked who it is")
	} else if connect.CodeOf(err) != connect.CodeUnauthenticated {
		t.Fatalf("an anonymous caller failed with %v, want unauthenticated", err)
	}
}

// makeUser makes a user and returns their id.
func makeUser(t *testing.T, opened *store.Store, name string, admin bool) string {
	t.Helper()

	user, err := opened.Queries().CreateUser(t.Context(), store.CreateUserParams{
		ID:    "01h" + name,
		Email: name + "@example.test",
		Name:  name,
		Admin: admin,
	})
	if err != nil {
		t.Fatalf("creating %s: %v", name, err)
	}
	return user.ID
}
