package audit_test

import (
	"log/slog"
	"net/http"
	"testing"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/access/accesstest"
	"github.com/aiviarium/rookery/internal/audit"
	"github.com/aiviarium/rookery/internal/spaces"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// start serves SpaceService and AuditService with the audit interceptor and the
// access interceptor, in that order.
func start(t *testing.T, opened *store.Store, principal access.Principal) (rookeryv1connect.SpaceServiceClient, rookeryv1connect.AuditServiceClient) {
	t.Helper()

	checker := access.NewChecker(opened)
	recorder := audit.NewRecorder(opened)
	spaceService := spaces.NewService(spaces.New(opened, checker), nil)
	auditService := audit.NewService(recorder)

	server := accesstest.New(t, checker, principal, func(mux *http.ServeMux, options []connect.HandlerOption) {
		path, handler := rookeryv1connect.NewSpaceServiceHandler(spaceService, options...)
		mux.Handle(path, handler)
		path, handler = rookeryv1connect.NewAuditServiceHandler(auditService, options...)
		mux.Handle(path, handler)
	}, connect.WithInterceptors(audit.NewInterceptor(recorder, slog.New(slog.DiscardHandler))))

	return rookeryv1connect.NewSpaceServiceClient(server.Client(), server.URL, connect.WithGRPC()),
		rookeryv1connect.NewAuditServiceClient(server.Client(), server.URL, connect.WithGRPC())
}

func TestTheTrailKeepsChangesAndRefusals(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	ctx := t.Context()
	admin := "01hadmin"
	if _, err := opened.Queries().CreateUser(ctx, store.CreateUserParams{ID: admin, Email: "admin@example.test", Admin: true}); err != nil {
		t.Fatalf("creating the admin: %v", err)
	}
	bob := "01hbob"
	if _, err := opened.Queries().CreateUser(ctx, store.CreateUserParams{ID: bob, Email: "bob@example.test"}); err != nil {
		t.Fatalf("creating bob: %v", err)
	}

	// Alice makes a space, which is a call at the use level: not one the trail
	// keeps. Its rename needs manage, and is.
	alice := "01halice"
	if _, err := opened.Queries().CreateUser(ctx, store.CreateUserParams{ID: alice, Email: "alice@example.test"}); err != nil {
		t.Fatalf("creating alice: %v", err)
	}
	made, err := spaces.New(opened, access.NewChecker(opened)).Create(ctx, alice, "general", "Team")
	if err != nil {
		t.Fatalf("making a space: %v", err)
	}

	asAlice, _ := start(t, opened, accesstest.User(alice, false))
	if _, err := asAlice.Rename(ctx, connect.NewRequest(&rookeryv1.SpaceServiceRenameRequest{Space: made.ID, Name: "The Team"})); err != nil {
		t.Fatalf("renaming: %v", err)
	}

	// Bob is refused the same call, and the refusal is in the trail.
	asBob, _ := start(t, opened, accesstest.User(bob, false))
	if _, err := asBob.Rename(ctx, connect.NewRequest(&rookeryv1.SpaceServiceRenameRequest{Space: made.ID, Name: "Bob's"})); err == nil {
		t.Fatal("bob renamed alice's space")
	}

	_, asAdmin := start(t, opened, accesstest.User(admin, true))
	listed, err := asAdmin.List(ctx, connect.NewRequest(&rookeryv1.AuditServiceListRequest{}))
	if err != nil {
		t.Fatalf("listing the trail: %v", err)
	}
	entries := listed.Msg.GetEntries()
	if len(entries) < 2 {
		t.Fatalf("the trail has %d entries, want the rename and the refusal", len(entries))
	}

	var sawOK, sawDenied bool
	for _, entry := range entries {
		switch entry.GetOutcome() {
		case audit.OK:
			sawOK = true
			if entry.GetResource() != access.SpaceURI(made.ID) {
				t.Errorf("the changed call names the resource %q", entry.GetResource())
			}
			if entry.GetPrincipal() != access.UserPrincipal(alice) {
				t.Errorf("the changed call names %q", entry.GetPrincipal())
			}
		case audit.Denied:
			sawDenied = true
			if entry.GetPrincipal() != access.UserPrincipal(bob) {
				t.Errorf("the refusal names %q", entry.GetPrincipal())
			}
			if entry.GetDetail() == "" {
				t.Error("the refusal says nothing about why")
			}
		}
	}
	if !sawOK || !sawDenied {
		t.Fatalf("the trail has no %v entry: ok %v, denied %v", "both", sawOK, sawDenied)
	}
}

func TestTheTrailIsAnAdmins(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	ctx := t.Context()
	user := "01halice"
	if _, err := opened.Queries().CreateUser(ctx, store.CreateUserParams{ID: user, Email: "alice@example.test"}); err != nil {
		t.Fatalf("creating alice: %v", err)
	}

	_, asAlice := start(t, opened, accesstest.User(user, false))
	if _, err := asAlice.List(ctx, connect.NewRequest(&rookeryv1.AuditServiceListRequest{})); err == nil {
		t.Fatal("a user who is not an admin read the trail")
	} else if connect.CodeOf(err) != connect.CodePermissionDenied {
		t.Fatalf("a user who is not an admin failed with %v, want permission denied", err)
	}
}

func TestFilterTheTrail(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	recorder := audit.NewRecorder(opened)
	ctx := t.Context()

	for _, entry := range []audit.Entry{
		{Principal: "user:01ha", Procedure: "rookery.v1.SpaceService/Rename", Resource: "space://01hx", Outcome: audit.OK},
		{Principal: "user:01hb", Procedure: "rookery.v1.AccessService/Grant", Resource: "space://01hx", Outcome: audit.OK},
	} {
		if err := recorder.Record(ctx, entry); err != nil {
			t.Fatalf("recording: %v", err)
		}
	}

	cases := []struct {
		name   string
		filter audit.Filter
		want   int
	}{
		{name: "all", filter: audit.Filter{}, want: 2},
		{name: "by principal", filter: audit.Filter{Principal: "user:01ha"}, want: 1},
		{name: "by procedure", filter: audit.Filter{Procedure: "rookery.v1.AccessService/Grant"}, want: 1},
		{name: "by resource", filter: audit.Filter{Resource: "space://01hx"}, want: 2},
		{name: "by nothing that matches", filter: audit.Filter{Principal: "user:01hc"}, want: 0},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			entries, err := recorder.List(ctx, tc.filter)
			if err != nil {
				t.Fatalf("listing: %v", err)
			}
			if len(entries) != tc.want {
				t.Fatalf("List returned %d entries, want %d", len(entries), tc.want)
			}
		})
	}
}
