package provision_test

import (
	"errors"
	"net/http"
	"strings"
	"testing"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/access/accesstest"
	"github.com/aiviarium/rookery/internal/provision"
	"github.com/aiviarium/rookery/internal/store"
)

// newService starts ProvisionerService for a test database and a caller.
func newService(t *testing.T, opened *store.Store, principal access.Principal, d *deployment) (*accesstest.Server, rookeryv1connect.ProvisionerServiceClient) {
	t.Helper()

	service := provision.NewService(d.provisioners)
	server := accesstest.New(t, access.NewChecker(opened), principal,
		func(mux *http.ServeMux, options []connect.HandlerOption) {
			path, handler := rookeryv1connect.NewProvisionerServiceHandler(service, options...)
			mux.Handle(path, handler)
		})
	return server, rookeryv1connect.NewProvisionerServiceClient(server.Client(), server.URL, connect.WithGRPC())
}

// admin is a user with the admin flag.
func admin(id string) access.Principal {
	return access.Principal{Kind: access.KindUser, ID: id, Email: id + "@example.test", Admin: true}
}

// TestListTypes checks what a provisioner says about itself reaches the edge.
func TestListTypes(t *testing.T) {
	t.Parallel()

	opened := testStore(t)
	d := newDeployment(t, opened)
	_, client := newService(t, opened, admin("admin-1"), d)

	types, err := client.ListTypes(t.Context(), connect.NewRequest(&rookeryv1.ProvisionerServiceListTypesRequest{}))
	if err != nil {
		t.Fatalf("listing the types: %v", err)
	}
	if len(types.Msg.GetTypes()) != 1 {
		t.Fatalf("the types are %+v", types.Msg.GetTypes())
	}
	typ := types.Msg.GetTypes()[0]
	if typ.GetId() != "example/fake" || typ.GetPlugin() != "example" || typ.GetLabel() != "Fake machine" {
		t.Fatalf("the type is %+v", typ)
	}
	if len(typ.GetKinds()) != 1 || !typ.GetKinds()[0].GetMachine() {
		t.Fatalf("the kinds are %+v", typ.GetKinds())
	}
	if len(typ.GetArguments()) != 2 || typ.GetArguments()[0].GetName() != "dir" || !typ.GetArguments()[0].GetRequired() {
		t.Fatalf("the arguments are %+v", typ.GetArguments())
	}
	if len(typ.GetActions()) != 3 || typ.GetActions()[2].GetName() != "fail" || !typ.GetActions()[2].GetDestructive() {
		t.Fatalf("the actions are %+v", typ.GetActions())
	}
	if !typ.GetNode() || !typ.GetLocal() {
		t.Fatalf("the type does not say where it runs: %+v", typ)
	}

	// Only an admin lists the types: they say what can be made, not what a
	// caller was given.
	_, stranger := newService(t, opened, accesstest.User("stranger", false), d)
	if _, err := stranger.ListTypes(t.Context(), connect.NewRequest(&rookeryv1.ProvisionerServiceListTypesRequest{})); connect.CodeOf(err) != connect.CodePermissionDenied {
		t.Fatalf("a stranger's ListTypes gave %v", err)
	}
}

// TestAddUpdateRemoveEntry checks the entry's life through the edge.
func TestAddUpdateRemoveEntry(t *testing.T) {
	t.Parallel()

	opened := testStore(t)
	d := newDeployment(t, opened)
	_, client := newService(t, opened, admin("admin-1"), d)
	ctx := t.Context()

	// A required argument that is missing is refused before the entry is made.
	if _, err := client.Add(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceAddRequest{
		Name:     "lab",
		Type:     "example/fake",
		ArgsCbor: mustMarshal(t, map[string]string{"address": "192.0.2.20"}),
	})); connect.CodeOf(err) != connect.CodeInvalidArgument {
		t.Fatalf("adding without the required argument gave %v", err)
	}
	if _, err := client.Add(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceAddRequest{
		Name:     "lab",
		Type:     "example/fake",
		ArgsCbor: mustMarshal(t, map[string]string{"dir": "/var/lib/fake", "nonsense": "x"}),
	})); connect.CodeOf(err) != connect.CodeInvalidArgument {
		t.Fatalf("adding an unknown argument gave %v", err)
	}
	if _, err := client.Add(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceAddRequest{
		Name: "lab",
		Type: "example/none",
	})); connect.CodeOf(err) != connect.CodeNotFound {
		t.Fatalf("adding an undeclared type gave %v", err)
	}

	added, err := client.Add(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceAddRequest{
		Name:     "lab",
		Type:     "example/fake",
		ArgsCbor: d.args,
		Node:     "node-1",
	}))
	if err != nil {
		t.Fatalf("adding the entry: %v", err)
	}
	if added.Msg.GetProvisioner().GetName() != "lab" || added.Msg.GetProvisioner().GetPlugin() != "example" {
		t.Fatalf("the entry is %+v", added.Msg.GetProvisioner())
	}

	got, err := client.Get(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceGetRequest{Provisioner: "lab"}))
	if err != nil {
		t.Fatalf("getting the entry: %v", err)
	}
	if got.Msg.GetProvisioner().GetNode() != "node-1" {
		t.Fatalf("the entry is %+v", got.Msg.GetProvisioner())
	}

	listed, err := client.List(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceListRequest{}))
	if err != nil {
		t.Fatalf("listing the entries: %v", err)
	}
	if len(listed.Msg.GetProvisioners()) != 1 {
		t.Fatalf("the entries are %+v", listed.Msg.GetProvisioners())
	}

	updated, err := client.Update(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceUpdateRequest{
		Provisioner: "lab",
		ArgsCbor:    mustMarshal(t, map[string]string{"dir": "/srv/fake"}),
	}))
	if err != nil {
		t.Fatalf("updating the entry: %v", err)
	}
	if updated.Msg.GetProvisioner().GetNode() != "node-1" {
		t.Fatalf("the update moved the entry: %+v", updated.Msg.GetProvisioner())
	}

	if _, err := client.Remove(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceRemoveRequest{Provisioner: "lab"})); err != nil {
		t.Fatalf("removing the entry: %v", err)
	}
	if _, err := client.Get(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceGetRequest{Provisioner: "lab"})); connect.CodeOf(err) != connect.CodeNotFound {
		t.Fatalf("a removed entry read as %v", err)
	}
}

// TestStatusAndActions checks the calls that reach the engine.
func TestStatusAndActions(t *testing.T) {
	t.Parallel()

	opened := testStore(t)
	d := newDeployment(t, opened)
	d.addLab(t)
	_, client := newService(t, opened, admin("admin-1"), d)
	ctx := t.Context()

	status, err := client.Status(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceStatusRequest{Provisioner: "lab"}))
	if err != nil {
		t.Fatalf("reading the status: %v", err)
	}
	if !status.Msg.GetUp() {
		t.Fatalf("the fake's status is %+v", status.Msg)
	}
	// The message carries what the process was given: its secrets and the
	// server's socket.
	if !strings.Contains(status.Msg.GetMessage(), "token set") || !strings.Contains(status.Msg.GetMessage(), "socket set") || !strings.Contains(status.Msg.GetMessage(), "node node-1") {
		t.Fatalf("the status message is %q", status.Msg.GetMessage())
	}

	// A machine, so that the action has something to report.
	if _, err := d.provisioners.CreateMachine(ctx, provision.MachineRequest{
		Host:  "desk-5",
		Entry: "lab",
		Seed:  provision.Seed{PublicKey: "ssh-ed25519 AAAAtest rookery"},
	}); err != nil {
		t.Fatalf("creating the machine: %v", err)
	}

	stream, err := client.RunAction(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceRunActionRequest{Provisioner: "lab", Action: "machines"}))
	if err != nil {
		t.Fatalf("running the action: %v", err)
	}
	var lines []string
	for stream.Receive() {
		lines = append(lines, stream.Msg().GetLine())
	}
	if err := stream.Err(); err != nil {
		t.Fatalf("running the action: %v", err)
	}
	if len(lines) != 1 || lines[0] != "desk-5" {
		t.Fatalf("the action's lines are %v", lines)
	}

	// An action that fails fails the stream.
	failed, err := client.RunAction(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceRunActionRequest{Provisioner: "lab", Action: "fail"}))
	if err != nil {
		t.Fatalf("running the failing action: %v", err)
	}
	for failed.Receive() {
	}
	if err := failed.Err(); err == nil {
		t.Fatal("the failing action did not fail")
	}

	resources, err := client.ListResources(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceListResourcesRequest{
		Provisioner: "lab",
		Kind:        provision.KindMachine,
	}))
	if err != nil {
		t.Fatalf("listing the resources: %v", err)
	}
	if len(resources.Msg.GetResources()) != 1 || resources.Msg.GetResources()[0].GetName() != "desk-5" {
		t.Fatalf("the resources are %+v", resources.Msg.GetResources())
	}

	// The provisioner's own list is what the labels it set answer, which is how
	// the core compares its records with the engine.
	made, err := d.provisioners.ListMade(ctx, "lab", provision.KindMachine)
	if err != nil {
		t.Fatalf("listing what the entry made: %v", err)
	}
	if len(made) != 1 || made[0].Name != "desk-5" || made[0].Labels["user.rookery.entry"] != "lab" {
		t.Fatalf("the provisioner made %+v", made)
	}
}

// TestEntryAccess checks that an entry is an admin's to add and the caller's to
// see once granted.
func TestEntryAccess(t *testing.T) {
	t.Parallel()

	opened := testStore(t)
	d := newDeployment(t, opened)
	d.addLab(t)
	_, stranger := newService(t, opened, accesstest.User("stranger", false), d)
	ctx := t.Context()

	if _, err := stranger.Get(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceGetRequest{Provisioner: "lab"})); connect.CodeOf(err) != connect.CodePermissionDenied {
		t.Fatalf("a stranger read an entry: %v", err)
	}
	if _, err := stranger.Add(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceAddRequest{Name: "other", Type: "example/fake", ArgsCbor: d.args})); connect.CodeOf(err) != connect.CodePermissionDenied {
		t.Fatalf("a stranger added an entry: %v", err)
	}

	if _, err := access.NewChecker(opened).Grant(ctx, access.ProvisionerURI("lab"), access.UserPrincipal("stranger"), access.Use); err != nil {
		t.Fatalf("granting use: %v", err)
	}
	if _, err := stranger.Get(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceGetRequest{Provisioner: "lab"})); err != nil {
		t.Fatalf("a granted stranger read the entry: %v", err)
	}

	// Use is not manage: running an action is the admin's, and so is removing it.
	// A stream is refused when it is read, not when it is opened.
	stream, err := stranger.RunAction(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceRunActionRequest{Provisioner: "lab", Action: "machines"}))
	if err == nil {
		for stream.Receive() {
		}
		err = stream.Err()
	}
	if connect.CodeOf(err) != connect.CodePermissionDenied {
		t.Fatalf("a stranger ran an action: %v", err)
	}
	if _, err := stranger.Remove(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceRemoveRequest{Provisioner: "lab"})); connect.CodeOf(err) != connect.CodePermissionDenied {
		t.Fatalf("a stranger removed an entry: %v", err)
	}
}

// TestEntryListsForTheGrantees checks that List is what the caller may view.
func TestEntryListsForTheGrantees(t *testing.T) {
	t.Parallel()

	opened := testStore(t)
	d := newDeployment(t, opened)
	d.addLab(t)
	_, stranger := newService(t, opened, accesstest.User("stranger", false), d)

	listed, err := stranger.List(t.Context(), connect.NewRequest(&rookeryv1.ProvisionerServiceListRequest{}))
	if err != nil {
		t.Fatalf("listing the entries: %v", err)
	}
	if len(listed.Msg.GetProvisioners()) != 0 {
		t.Fatalf("a stranger sees %+v", listed.Msg.GetProvisioners())
	}
}

// TestAddWithoutASeedFails checks that a machine's creation fails when the seed
// cannot be rendered.
func TestCreateMachineRefusesABadRequest(t *testing.T) {
	t.Parallel()

	opened := testStore(t)
	d := newDeployment(t, opened)
	d.addLab(t)

	if _, err := d.provisioners.CreateMachine(t.Context(), provision.MachineRequest{
		Host:  "not a name",
		Entry: "lab",
		Seed:  provision.Seed{PublicKey: "ssh-ed25519 AAAAtest rookery"},
	}); !errors.Is(err, access.ErrInvalid) {
		t.Fatalf("creating a machine called %q gave %v", "not a name", err)
	}
	if _, err := d.provisioners.CreateMachine(t.Context(), provision.MachineRequest{
		Host:  "desk-6",
		Entry: "lab",
	}); !errors.Is(err, access.ErrInvalid) {
		t.Fatalf("creating a machine without a key gave %v", err)
	}
	if err := d.provisioners.RemoveMachine(t.Context(), "nothing", nil); !errors.Is(err, access.ErrNotFound) {
		t.Fatalf("removing a host with no machine gave %v", err)
	}
}
