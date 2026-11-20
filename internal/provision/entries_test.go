package provision_test

import (
	"errors"
	"strings"
	"testing"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/provision"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// testStore is the test's database.
func testStore(t *testing.T) *store.Store {
	t.Helper()
	return storetest.New(t)
}

// TestEntries writes, reads and removes an entry.
func TestEntries(t *testing.T) {
	t.Parallel()

	opened := testStore(t)
	entries := provision.NewEntries(opened, access.NewChecker(opened))
	ctx := t.Context()

	args := mustMarshal(t, map[string]string{"dir": "/var/lib/fake"})
	added, err := entries.Add(ctx, provision.Entry{
		Name:   "lab",
		Type:   "example/fake",
		Plugin: "example",
		Args:   args,
		Node:   "node-1",
	})
	if err != nil {
		t.Fatalf("adding the entry: %v", err)
	}
	if added.Name != "lab" || added.Node != "node-1" || added.Local() {
		t.Fatalf("the entry is %+v", added)
	}

	read, err := entries.ByName(ctx, "lab")
	if err != nil {
		t.Fatalf("reading the entry: %v", err)
	}
	if string(read.Args) != string(args) || read.Type != "example/fake" {
		t.Fatalf("the entry read back as %+v", read)
	}

	// An admin's list has it; a stranger's does not, until it is granted.
	admin := access.Principal{Kind: access.KindUser, ID: "admin-1", Admin: true}
	list, err := entries.List(ctx, admin, "", 10)
	if err != nil {
		t.Fatalf("listing entries: %v", err)
	}
	if len(list) != 1 {
		t.Fatalf("the admin sees %d entries", len(list))
	}
	stranger := access.Principal{Kind: access.KindUser, ID: "stranger"}
	list, err = entries.List(ctx, stranger, "", 10)
	if err != nil {
		t.Fatalf("listing entries as a stranger: %v", err)
	}
	if len(list) != 0 {
		t.Fatalf("a stranger sees %d entries", len(list))
	}
	if _, err := access.NewChecker(opened).Grant(ctx, access.ProvisionerURI("lab"), access.UserPrincipal("stranger"), access.View); err != nil {
		t.Fatalf("granting view: %v", err)
	}
	list, err = entries.List(ctx, stranger, "", 10)
	if err != nil {
		t.Fatalf("listing entries as a stranger again: %v", err)
	}
	if len(list) != 1 {
		t.Fatalf("a granted stranger sees %d entries", len(list))
	}

	updated, err := entries.Update(ctx, "lab", mustMarshal(t, map[string]string{"dir": "/srv/fake"}), "", "")
	if err != nil {
		t.Fatalf("updating the entry: %v", err)
	}
	if updated.Node != "node-1" || string(updated.Args) == string(args) {
		t.Fatalf("the updated entry is %+v", updated)
	}

	// A name is one entry.
	if _, err := entries.Add(ctx, provision.Entry{Name: "lab", Type: "example/fake", Plugin: "example", Args: args}); !errors.Is(err, access.ErrConflict) {
		t.Fatalf("adding the same name gave %v", err)
	}

	// Bad entries are refused before they reach the database.
	for name, entry := range map[string]provision.Entry{
		"no name":            {Type: "example/fake"},
		"no type":            {Name: "x"},
		"not a type":         {Name: "x", Type: "fake"},
		"a slash at the end": {Name: "a/", Type: "example/fake"},
		"node+remote":        {Name: "x", Type: "example/fake", Node: "n", Remote: "https://example.test"},
		"name is long":       {Name: strings.Repeat("a", 65), Type: "example/fake"},
	} {
		if _, err := entries.Add(ctx, entry); !errors.Is(err, access.ErrInvalid) {
			t.Errorf("%s: %v", name, err)
		}
	}

	if err := entries.Remove(ctx, "lab"); err != nil {
		t.Fatalf("removing the entry: %v", err)
	}
	if _, err := entries.ByName(ctx, "lab"); !errors.Is(err, access.ErrNotFound) {
		t.Fatalf("the entry is still there: %v", err)
	}
}

// TestRemoveRefusesWithResources checks that an entry with machines is not
// removed under them.
func TestRemoveRefusesWithResources(t *testing.T) {
	t.Parallel()

	opened := testStore(t)
	d := newDeployment(t, opened)
	d.addLab(t)

	if _, err := d.provisioners.CreateMachine(t.Context(), provision.MachineRequest{
		Host:  "desk-4",
		Entry: "lab",
		Seed:  provision.Seed{PublicKey: "ssh-ed25519 AAAAtest rookery"},
	}); err != nil {
		t.Fatalf("creating the machine: %v", err)
	}
	if err := d.provisioners.Entries().Remove(t.Context(), "lab"); !errors.Is(err, access.ErrConflict) {
		t.Fatalf("removing an entry with a machine gave %v", err)
	}
}

// TestOffer adds the entries a feature offers when it is applied to a node.
func TestOffer(t *testing.T) {
	t.Parallel()

	opened := testStore(t)
	d := newDeployment(t, opened)

	added, err := d.provisioners.Offer(t.Context(), "node-1", []string{"example/fake"})
	if err != nil {
		t.Fatalf("offering: %v", err)
	}
	if len(added) != 1 || added[0].Name != "node-1-example/fake" || added[0].Node != "node-1" {
		t.Fatalf("the offered entries are %+v", added)
	}

	// Offering again keeps the one entry.
	again, err := d.provisioners.Offer(t.Context(), "node-1", []string{"example/fake"})
	if err != nil {
		t.Fatalf("offering again: %v", err)
	}
	if len(again) != 1 || again[0].Name != added[0].Name {
		t.Fatalf("the offered entries are %+v", again)
	}

	if _, err := d.provisioners.Offer(t.Context(), "node-2", []string{"example/none"}); !errors.Is(err, provision.ErrNoType) {
		t.Fatalf("offering a type nothing declares gave %v", err)
	}
}
