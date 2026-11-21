package access_test

import (
	"context"
	"errors"
	"testing"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/reflect/protoreflect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

func TestEveryProcedureDeclaresAccess(t *testing.T) {
	t.Parallel()

	files := []protoreflect.FileDescriptor{
		rookeryv1.File_rookery_v1_access_proto,
		rookeryv1.File_rookery_v1_access_service_proto,
		rookeryv1.File_rookery_v1_audit_proto,
		rookeryv1.File_rookery_v1_auth_proto,
		rookeryv1.File_rookery_v1_data_proto,
		rookeryv1.File_rookery_v1_desktop_proto,
		rookeryv1.File_rookery_v1_documents_proto,
		rookeryv1.File_rookery_v1_entities_proto,
		rookeryv1.File_rookery_v1_group_proto,
		rookeryv1.File_rookery_v1_local_proto,
		rookeryv1.File_rookery_v1_space_proto,
		rookeryv1.File_rookery_v1_user_proto,
	}

	count := 0
	for _, method := range access.Methods(files...) {
		count++
		level, field, err := access.MethodOption(method)
		if err != nil {
			t.Errorf("%s: %v", method.FullName(), err)
			continue
		}
		if level == rookeryv1.Level_LEVEL_UNSPECIFIED {
			t.Errorf("%s declares no level", method.FullName())
		}
		if field != "" && method.Input().Fields().ByName(protoreflect.Name(field)) == nil {
			t.Errorf("%s needs access on the field %q, which its request does not have", method.FullName(), field)
		}
	}
	if count == 0 {
		t.Fatal("no procedures were walked: the test checks nothing")
	}
}

// TestChecker covers the levels a principal holds, with real grants.
func TestChecker(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	checker := access.NewChecker(opened)
	ctx := t.Context()

	admin := access.Principal{Kind: access.KindUser, ID: "01hadmin", Admin: true}
	alice := access.Principal{Kind: access.KindUser, ID: "01halice"}
	bob := access.Principal{Kind: access.KindUser, ID: "01hbob"}
	anonymous := access.Principal{}

	// The space, with alice its creator.
	if _, err := checker.Grant(ctx, access.SpaceURI("01hspace"), access.UserPrincipal(alice.ID), access.Manage); err != nil {
		t.Fatalf("granting alice manage: %v", err)
	}
	// A record in the space: a grant on the space reaches it.
	record := access.RecordURI("example", "notes", "01hnote")
	if err := checker.SetParent(ctx, record, access.SpaceURI("01hspace")); err != nil {
		t.Fatalf("setting the record's parent: %v", err)
	}
	// Bob is in a group that holds use on a host.
	if _, err := checker.Grant(ctx, access.HostURI("forge"), access.GroupPrincipal("01hgroup"), access.Use); err != nil {
		t.Fatalf("granting the group use: %v", err)
	}
	if err := setGroupMember(ctx, t, opened, alice.ID, "01hgroup"); err != nil {
		t.Fatalf("adding alice to the group: %v", err)
	}
	// Everyone holds view on another host.
	if _, err := checker.Grant(ctx, access.HostURI("public"), access.Everyone, access.View); err != nil {
		t.Fatalf("granting everyone view: %v", err)
	}

	cases := []struct {
		name      string
		principal access.Principal
		resource  string
		need      access.Level
		want      bool
	}{
		{name: "creator holds manage", principal: alice, resource: access.SpaceURI("01hspace"), need: access.Manage, want: true},
		{name: "creator holds view", principal: alice, resource: access.SpaceURI("01hspace"), need: access.View, want: true},
		{name: "parent grant reaches the child", principal: alice, resource: record, need: access.Manage, want: true},
		{name: "a stranger holds nothing", principal: bob, resource: access.SpaceURI("01hspace"), need: access.View, want: false},
		{name: "group holds use", principal: alice, resource: access.HostURI("forge"), need: access.Use, want: true},
		{name: "group does not hold manage", principal: alice, resource: access.HostURI("forge"), need: access.Manage, want: false},
		{name: "everyone holds view", principal: bob, resource: access.HostURI("public"), need: access.View, want: true},
		{name: "everyone does not hold use", principal: bob, resource: access.HostURI("public"), need: access.Use, want: false},
		{name: "an admin holds manage everywhere", principal: admin, resource: access.HostURI("forge"), need: access.Manage, want: true},
		{name: "the local admin holds manage everywhere", principal: access.LocalAdmin(), resource: access.HostURI("forge"), need: access.Manage, want: true},
		{name: "an anonymous caller holds nothing", principal: anonymous, resource: access.HostURI("public"), need: access.View, want: false},
		{name: "no resource needs an admin for manage", principal: bob, resource: "", need: access.Manage, want: false},
		{name: "no resource is fine for view", principal: bob, resource: "", need: access.View, want: true},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			err := checker.Check(ctx, tc.principal, tc.resource, tc.need)
			if got := err == nil; got != tc.want {
				t.Fatalf("Check(%s, %s, %s) = %v, want allowed %v", tc.principal, tc.resource, access.LevelName(tc.need), err, tc.want)
			}
			if !tc.want && err != nil {
				var denied *access.Denied
				if !errors.As(err, &denied) && !errors.Is(err, access.ErrUnauthenticated) {
					t.Fatalf("a refusal is a *access.Denied: %T", err)
				}
			}
		})
	}
}

func TestGrantRefusesWhatIsNotAGrant(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	checker := access.NewChecker(opened)
	ctx := t.Context()

	cases := []struct {
		name      string
		resource  string
		principal string
		level     access.Level
	}{
		{name: "not a URI", resource: "forge", principal: "user:01h", level: access.View},
		{name: "an unknown kind", resource: "rocket://one", principal: "user:01h", level: access.View},
		{name: "not a principal", resource: access.HostURI("forge"), principal: "forge", level: access.View},
		{name: "not a grantable level", resource: access.HostURI("forge"), principal: "user:01h", level: access.Admin},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			if _, err := checker.Grant(ctx, tc.resource, tc.principal, tc.level); err == nil {
				t.Fatalf("Grant(%q, %q, %s) was accepted", tc.resource, tc.principal, access.LevelName(tc.level))
			}
		})
	}
}

func TestToConnect(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name string
		err  error
		code connect.Code
	}{
		{name: "nil", err: nil},
		{name: "invalid", err: access.ErrInvalid, code: connect.CodeInvalidArgument},
		{name: "unauthenticated", err: access.ErrUnauthenticated, code: connect.CodeUnauthenticated},
		{name: "denied", err: access.ErrDenied, code: connect.CodePermissionDenied},
		{name: "a refusal", err: &access.Denied{Resource: access.HostURI("forge"), Need: access.Use}, code: connect.CodePermissionDenied},
		{name: "not found", err: access.ErrNotFound, code: connect.CodeNotFound},
		{name: "conflict", err: access.ErrConflict, code: connect.CodeAborted},
		{name: "unavailable", err: access.ErrUnavailable, code: connect.CodeUnavailable},
		{name: "deadline", err: context.DeadlineExceeded, code: connect.CodeDeadlineExceeded},
		{name: "cancelled", err: context.Canceled, code: connect.CodeCanceled},
		{name: "wrapped", err: wrap(access.ErrNotFound, "no user"), code: connect.CodeNotFound},
		{name: "other", err: errors.New("something"), code: connect.CodeInternal},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			mapped := access.ToConnect(tc.err)
			if tc.err == nil {
				if mapped != nil {
					t.Fatalf("ToConnect(nil) = %v", mapped)
				}
				return
			}
			var connectErr *connect.Error
			if !errors.As(mapped, &connectErr) {
				t.Fatalf("ToConnect returned %T, not a connect error", mapped)
			}
			if connectErr.Code() != tc.code {
				t.Errorf("ToConnect(%v) has code %v, want %v", tc.err, connectErr.Code(), tc.code)
			}
		})
	}
}

func TestRefusalCarriesItsDetail(t *testing.T) {
	t.Parallel()

	denied := &access.Denied{Resource: access.HostURI("forge"), Need: access.Use, Reason: "been given use"}
	mapped := access.ToConnect(denied)

	refusal := access.RefusalOf(mapped)
	if refusal == nil {
		t.Fatal("the refusal carries no rookery.v1.Refusal")
	}
	if refusal.GetResource() != access.HostURI("forge") {
		t.Errorf("the refusal names %q", refusal.GetResource())
	}
	if refusal.GetLevel() != access.Use {
		t.Errorf("the refusal asks for %v", refusal.GetLevel())
	}
	if refusal.GetReason() == "" {
		t.Error("the refusal says no reason")
	}
}

// setGroupMember makes a group and a user and puts the user in it.
func setGroupMember(ctx context.Context, t *testing.T, opened *store.Store, userID, groupID string) error {
	t.Helper()
	if _, err := opened.Queries().CreateGroup(ctx, store.CreateGroupParams{ID: groupID, Name: "writers"}); err != nil {
		return err
	}
	if _, err := opened.Queries().CreateUser(ctx, store.CreateUserParams{ID: userID, Email: userID + "@example.test"}); err != nil {
		return err
	}
	return opened.Queries().AddGroupMember(ctx, store.AddGroupMemberParams{
		GroupID: groupID,
		UserID:  userID,
		Source:  "manual",
	})
}

// wrap is fmt.Errorf with one wrapped error.
func wrap(err error, text string) error {
	return &wrapped{err: err, text: text}
}

type wrapped struct {
	err  error
	text string
}

func (w *wrapped) Error() string { return w.text + ": " + w.err.Error() }
func (w *wrapped) Unwrap() error { return w.err }
