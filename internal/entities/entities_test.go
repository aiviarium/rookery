package entities

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/data"
	"github.com/aiviarium/rookery/internal/filter"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// testAuth is an Authorizer the test grants by hand.
type testAuth struct {
	mu     sync.Mutex
	grants map[string]rookeryv1.Level
}

// Level is the principal's granted level on the resource.
func (a *testAuth) Level(_ context.Context, principal data.Principal, resource string) (rookeryv1.Level, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.grants[principal.String()+"\x00"+resource], nil
}

// grant gives a principal a level on a resource.
func (a *testAuth) grant(principal data.Principal, resource string, level rookeryv1.Level) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.grants[principal.String()+"\x00"+resource] = level
}

// entityDatastore is a datastore with one entity collection and three notes.
type entityDatastore struct {
	data     *data.Data
	auth     *testAuth
	registry *Registry
	user     data.Principal
	other    data.Principal
	space    string
	notes    map[string]string
}

// use is a shared datastore across the tests of this file: the setup is the
// same and the tests only read it.
func use(t *testing.T) *entityDatastore {
	t.Helper()

	auth := &testAuth{grants: make(map[string]rookeryv1.Level)}
	d := data.New(data.Config{Store: storetest.New(t), Authorizer: auth})
	err := d.Declare(&data.Collection{
		Plugin: "example",
		Name:   "notes",
		Scope:  data.ScopeSpace,
		Access: data.Access{Read: rookeryv1.Level_LEVEL_VIEW, Write: rookeryv1.Level_LEVEL_USE},
		Fields: map[string]data.Field{
			"title": {Type: filter.TypeText, Required: true},
			"body":  {Type: filter.TypeText},
		},
		Entity: &data.Entity{Kind: "Note", Title: "title", Text: []string{"title", "body"}},
	})
	if err != nil {
		t.Fatalf("declaring: %v", err)
	}
	if err := d.Create(t.Context(), "example"); err != nil {
		t.Fatalf("creating: %v", err)
	}

	user := data.Principal{Kind: data.PrincipalUser, ID: "01USER"}
	other := data.Principal{Kind: data.PrincipalUser, ID: "02USER"}
	auth.grant(user, "space://01SPACE", rookeryv1.Level_LEVEL_MANAGE)
	auth.grant(other, "space://01SPACE", rookeryv1.Level_LEVEL_MANAGE)

	notes := make(map[string]string)
	for _, note := range []struct{ key, title, body string }{
		{"rook", "Rook notes", "how the rookeries work"},
		{"crow", "Crow notes", "the shell and its plugins"},
		{"quiet", "Quiet notes", "nothing about birds"},
	} {
		written, err := d.Insert(t.Context(), user, "example/notes", map[string]any{
			"title": note.title, "body": note.body, "space_id": "01SPACE",
		})
		if err != nil {
			t.Fatalf("inserting %s: %v", note.key, err)
		}
		notes[note.key] = "record://example/notes/" + written["id"].(string)
	}

	return &entityDatastore{
		data:     d,
		auth:     auth,
		registry: New(Config{Data: d, Authorizer: auth}),
		user:     user,
		other:    other,
		space:    "01SPACE",
		notes:    notes,
	}
}

// embeds writes an embedding on an entity, as a plugin that has one would.
func (e *entityDatastore) embeds(t *testing.T, uri string, vector string) {
	t.Helper()
	_, err := e.data.Store().Pool().Exec(t.Context(),
		`UPDATE rookery.entities SET embedding = $1::vector WHERE uri = $2`, vector, uri)
	if err != nil {
		t.Fatalf("writing an embedding: %v", err)
	}
}

// TestKinds checks the declared kinds.
func TestKinds(t *testing.T) {
	t.Parallel()

	e := use(t)
	if collection, ok := e.registry.Kind("Note"); !ok || collection != "example/notes" {
		t.Fatalf("Kind(Note) = %q, %v", collection, ok)
	}
	if kinds := e.registry.Kinds(); len(kinds) != 1 || kinds[0] != "Note" {
		t.Fatalf("Kinds() = %v", kinds)
	}
	if _, ok := e.registry.Kind("Document"); ok {
		t.Error("an undeclared kind was found")
	}
}

// TestResolve checks resolution, including the access check.
func TestResolve(t *testing.T) {
	t.Parallel()

	e := use(t)
	ctx := t.Context()

	resolved, err := e.registry.Resolve(ctx, e.user, e.notes["rook"])
	if err != nil {
		t.Fatalf("resolving: %v", err)
	}
	if resolved.Kind != "Note" || resolved.Title != "Rook notes" || resolved.Collection != "example/notes" || resolved.Access != "space://01SPACE" {
		t.Errorf("the entity = %+v", resolved)
	}

	if _, err := e.registry.Resolve(ctx, e.user, "record://example/notes/missing"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("resolving a missing entity = %v, want ErrNotFound", err)
	}

	// A user with no level on the space sees nothing.
	stranger := data.Principal{Kind: data.PrincipalUser, ID: "03USER"}
	if _, err := e.registry.Resolve(ctx, stranger, e.notes["rook"]); !errors.Is(err, ErrForbidden) {
		t.Fatalf("resolving as a stranger = %v, want ErrForbidden", err)
	}
	found, err := e.registry.ResolveMany(ctx, stranger, []string{e.notes["rook"], e.notes["crow"]})
	if err != nil {
		t.Fatalf("resolving many as a stranger: %v", err)
	}
	if len(found) != 0 {
		t.Errorf("a stranger resolved %d entities", len(found))
	}

	many, err := e.registry.ResolveMany(ctx, e.user, []string{e.notes["rook"], e.notes["crow"], "record://example/notes/missing"})
	if err != nil {
		t.Fatalf("resolving many: %v", err)
	}
	if len(many) != 2 {
		t.Fatalf("ResolveMany found %d entities, want 2", len(many))
	}
}

// TestSearch checks the full text search and the access filter.
func TestSearch(t *testing.T) {
	t.Parallel()

	e := use(t)
	ctx := t.Context()

	found, err := e.registry.Search(ctx, e.user, "rook", nil, 10)
	if err != nil {
		t.Fatalf("searching: %v", err)
	}
	if len(found) != 1 || found[0].URI != e.notes["rook"] {
		t.Fatalf("searching for rook = %v", found)
	}
	if found[0].Collection != "example/notes" {
		t.Errorf("the collection is %q", found[0].Collection)
	}

	// Websearch syntax: notes, but not rooks.
	found, err = e.registry.Search(ctx, e.user, "notes -rook", nil, 10)
	if err != nil {
		t.Fatalf("searching: %v", err)
	}
	if len(found) != 2 {
		t.Fatalf("searching for notes but not rook = %v", found)
	}

	// Nothing matches.
	found, err = e.registry.Search(ctx, e.user, "penguin", nil, 10)
	if err != nil {
		t.Fatalf("searching: %v", err)
	}
	if len(found) != 0 {
		t.Fatalf("searching for a word no entity has = %v", found)
	}

	// A stranger sees nothing.
	stranger := data.Principal{Kind: data.PrincipalUser, ID: "03USER"}
	found, err = e.registry.Search(ctx, stranger, "notes", nil, 10)
	if err != nil {
		t.Fatalf("searching as a stranger: %v", err)
	}
	if len(found) != 0 {
		t.Fatalf("a stranger's search = %v", found)
	}

	// A search needs something to search for.
	if _, err := e.registry.Search(ctx, e.user, "  ", nil, 10); !errors.Is(err, ErrInvalid) {
		t.Fatalf("an empty search = %v, want ErrInvalid", err)
	}
}

// TestSearchEmbedding checks that a vector reorders the results.
func TestSearchEmbedding(t *testing.T) {
	t.Parallel()

	e := use(t)
	ctx := t.Context()

	// The crow note is nearest a vector close to its own.
	e.embeds(t, e.notes["rook"], "[1,0,0]")
	e.embeds(t, e.notes["crow"], "[0,1,0]")
	e.embeds(t, e.notes["quiet"], "[0,0,1]")

	found, err := e.registry.Search(ctx, e.user, "notes", []float32{0, 1, 0}, 10)
	if err != nil {
		t.Fatalf("searching with an embedding: %v", err)
	}
	if len(found) != 3 || found[0].URI != e.notes["crow"] {
		t.Fatalf("the embedding search = %v", found)
	}

	// With no text at all, the embedding alone orders the results.
	found, err = e.registry.Search(ctx, e.user, "", []float32{0, 0, 1}, 10)
	if err != nil {
		t.Fatalf("searching by embedding alone: %v", err)
	}
	if len(found) != 3 || found[0].URI != e.notes["quiet"] {
		t.Fatalf("the vector-only search = %v", found)
	}
}

// TestService checks the entity API over a connect client.
func TestService(t *testing.T) {
	t.Parallel()

	e := use(t)
	ctx := t.Context()

	path, handler := rookeryv1connect.NewEntityServiceHandler(NewService(e.registry, testCaller{e.user}))
	mux := http.NewServeMux()
	mux.Handle(path, handler)
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	client := rookeryv1connect.NewEntityServiceClient(server.Client(), server.URL, connect.WithGRPC())

	resolved, err := client.Resolve(ctx, connect.NewRequest(&rookeryv1.ResolveRequest{Uri: e.notes["rook"]}))
	if err != nil {
		t.Fatalf("resolving: %v", err)
	}
	if resolved.Msg.GetEntity().GetTitle() != "Rook notes" {
		t.Errorf("the entity = %v", resolved.Msg.GetEntity())
	}

	many, err := client.ResolveMany(ctx, connect.NewRequest(&rookeryv1.ResolveManyRequest{
		Uris: []string{e.notes["rook"], e.notes["crow"]},
	}))
	if err != nil {
		t.Fatalf("resolving many: %v", err)
	}
	if len(many.Msg.GetEntities()) != 2 {
		t.Errorf("ResolveMany returned %d entities", len(many.Msg.GetEntities()))
	}

	found, err := client.Search(ctx, connect.NewRequest(&rookeryv1.SearchRequest{Query: "notes", Limit: 10}))
	if err != nil {
		t.Fatalf("searching: %v", err)
	}
	if len(found.Msg.GetEntities()) != 3 {
		t.Errorf("searching for notes returned %d entities", len(found.Msg.GetEntities()))
	}

	// A missing entity is NOT_FOUND.
	_, err = client.Resolve(ctx, connect.NewRequest(&rookeryv1.ResolveRequest{Uri: "record://example/notes/missing"}))
	if connect.CodeOf(err) != connect.CodeNotFound {
		t.Errorf("resolving a missing entity = %v, want NOT_FOUND", err)
	}

	// An embedding that is not a list of numbers is INVALID_ARGUMENT.
	_, err = client.Search(ctx, connect.NewRequest(&rookeryv1.SearchRequest{
		Query: "notes", EmbeddingCbor: []byte{0xa1, 0x61, 0x61, 0x01},
	}))
	if connect.CodeOf(err) != connect.CodeInvalidArgument {
		t.Errorf("a malformed embedding = %v, want INVALID_ARGUMENT", err)
	}
}

// testCaller is a Caller that always returns one principal.
type testCaller struct{ principal data.Principal }

// Principal is the caller.
func (c testCaller) Principal(context.Context) (data.Principal, error) { return c.principal, nil }
