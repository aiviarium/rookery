package spaces_test

import (
	"net/http"
	"testing"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/access/accesstest"
	"github.com/aiviarium/rookery/internal/spaces"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// newService starts SpaceService for a test database and a caller.
func newService(t *testing.T, opened *store.Store, principal access.Principal) (*accesstest.Server, rookeryv1connect.SpaceServiceClient) {
	t.Helper()

	service := spaces.NewService(spaces.New(opened, access.NewChecker(opened)), nil)
	server := accesstest.New(t, access.NewChecker(opened), principal,
		func(mux *http.ServeMux, options []connect.HandlerOption) {
			path, handler := rookeryv1connect.NewSpaceServiceHandler(service, options...)
			mux.Handle(path, handler)
		})
	return server, rookeryv1connect.NewSpaceServiceClient(server.Client(), server.URL, connect.WithGRPC())
}

func TestCreateAndShareASpace(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	ctx := t.Context()
	alice := createUser(t, opened, "alice", false)
	bob := createUser(t, opened, "bob", false)

	_, asAlice := newService(t, opened, accesstest.User(alice, false))
	created, err := asAlice.Create(ctx, connect.NewRequest(&rookeryv1.SpaceServiceCreateRequest{Kind: "project", Name: "Kitchen"}))
	if err != nil {
		t.Fatalf("creating a space: %v", err)
	}
	spaceID := created.Msg.GetSpace().GetId()
	if spaceID == "" {
		t.Fatal("the new space has no id")
	}

	// The creator holds manage, through the grant the create made.
	checker := access.NewChecker(opened)
	if err := checker.Check(ctx, access.Principal{Kind: access.KindUser, ID: alice}, access.SpaceURI(spaceID), access.Manage); err != nil {
		t.Fatalf("the creator does not hold manage on their space: %v", err)
	}

	// Bob sees it only once he is granted view.
	_, asBob := newService(t, opened, accesstest.User(bob, false))
	list, err := asBob.List(ctx, connect.NewRequest(&rookeryv1.SpaceServiceListRequest{}))
	if err != nil {
		t.Fatalf("listing spaces: %v", err)
	}
	if len(list.Msg.GetSpaces()) != 0 {
		t.Fatalf("bob sees %d spaces he was not granted", len(list.Msg.GetSpaces()))
	}

	if _, err := asBob.Get(ctx, connect.NewRequest(&rookeryv1.SpaceServiceGetRequest{Space: spaceID})); err == nil {
		t.Fatal("bob read a space he was not granted")
	} else if connect.CodeOf(err) != connect.CodePermissionDenied {
		t.Fatalf("bob's read failed with %v, want permission denied", err)
	}

	if _, err := checker.Grant(ctx, access.SpaceURI(spaceID), access.UserPrincipal(bob), access.View); err != nil {
		t.Fatalf("granting bob view: %v", err)
	}

	list, err = asBob.List(ctx, connect.NewRequest(&rookeryv1.SpaceServiceListRequest{}))
	if err != nil {
		t.Fatalf("listing spaces: %v", err)
	}
	if len(list.Msg.GetSpaces()) != 1 {
		t.Fatalf("bob sees %d spaces, want 1", len(list.Msg.GetSpaces()))
	}

	// View is not manage: bob may not rename it.
	if _, err := asBob.Rename(ctx, connect.NewRequest(&rookeryv1.SpaceServiceRenameRequest{Space: spaceID, Name: "Bob's"})); err == nil {
		t.Fatal("bob renamed a space he may only view")
	} else if connect.CodeOf(err) != connect.CodePermissionDenied {
		t.Fatalf("bob's rename failed with %v, want permission denied", err)
	}
}

func TestArchiveAndDelete(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	ctx := t.Context()
	alice := createUser(t, opened, "alice", true)

	_, asAlice := newService(t, opened, accesstest.User(alice, true))
	created, err := asAlice.Create(ctx, connect.NewRequest(&rookeryv1.SpaceServiceCreateRequest{Kind: "general", Name: "Team"}))
	if err != nil {
		t.Fatalf("creating a space: %v", err)
	}
	spaceID := created.Msg.GetSpace().GetId()

	archived, err := asAlice.Archive(ctx, connect.NewRequest(&rookeryv1.SpaceServiceArchiveRequest{Space: spaceID, Archived: true}))
	if err != nil {
		t.Fatalf("archiving: %v", err)
	}
	if !archived.Msg.GetSpace().GetArchived() {
		t.Fatal("the space is not archived")
	}

	list, err := asAlice.List(ctx, connect.NewRequest(&rookeryv1.SpaceServiceListRequest{}))
	if err != nil {
		t.Fatalf("listing: %v", err)
	}
	if len(list.Msg.GetSpaces()) != 0 {
		t.Fatal("an archived space is in the list that leaves archived spaces out")
	}
	list, err = asAlice.List(ctx, connect.NewRequest(&rookeryv1.SpaceServiceListRequest{IncludeArchived: true}))
	if err != nil {
		t.Fatalf("listing with archived: %v", err)
	}
	if len(list.Msg.GetSpaces()) != 1 {
		t.Fatal("an archived space is not in the list that includes them")
	}

	if _, err := asAlice.Delete(ctx, connect.NewRequest(&rookeryv1.SpaceServiceDeleteRequest{Space: spaceID})); err != nil {
		t.Fatalf("deleting: %v", err)
	}
	if _, err := asAlice.Get(ctx, connect.NewRequest(&rookeryv1.SpaceServiceGetRequest{Space: spaceID})); err == nil {
		t.Fatal("a deleted space is still there")
	}
}

func TestKindsAndKindsOfSpaces(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	ctx := t.Context()
	alice := createUser(t, opened, "alice", false)

	_, asAlice := newService(t, opened, accesstest.User(alice, false))
	kinds, err := asAlice.ListKinds(ctx, connect.NewRequest(&rookeryv1.SpaceServiceListKindsRequest{}))
	if err != nil {
		t.Fatalf("listing kinds: %v", err)
	}
	if len(kinds.Msg.GetKinds()) == 0 {
		t.Fatal("the core declares no space kind")
	}

	if _, err := asAlice.Create(ctx, connect.NewRequest(&rookeryv1.SpaceServiceCreateRequest{Kind: "nonsense", Name: "Nope"})); err == nil {
		t.Fatal("a space of an undeclared kind was made")
	} else if connect.CodeOf(err) != connect.CodeInvalidArgument {
		t.Fatalf("an undeclared kind failed with %v, want invalid argument", err)
	}
}

func TestAnonymousCallerIsRefused(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)

	_, client := newService(t, opened, accesstest.Anonymous())
	if _, err := client.List(t.Context(), connect.NewRequest(&rookeryv1.SpaceServiceListRequest{})); err == nil {
		t.Fatal("an anonymous caller listed the spaces")
	} else if connect.CodeOf(err) != connect.CodeUnauthenticated {
		t.Fatalf("an anonymous caller failed with %v, want unauthenticated", err)
	}
}

// createUser makes a user and returns their id.
func createUser(t *testing.T, opened *store.Store, name string, admin bool) string {
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
