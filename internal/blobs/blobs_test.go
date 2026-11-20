package blobs_test

import (
	"bytes"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/blobs"
	storedb "github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// newBlobs returns a blob store on a directory backend and a test database.
func newBlobs(t *testing.T) (*storedb.Store, *blobs.Blobs) {
	t.Helper()

	opened := storetest.New(t)
	backend, err := blobs.NewDirectory(t.TempDir())
	if err != nil {
		t.Fatalf("making the backend: %v", err)
	}
	return opened, blobs.New(opened, backend, access.NewChecker(opened))
}

func TestBlobsAreTheirContent(t *testing.T) {
	t.Parallel()

	_, blobsStore := newBlobs(t)
	ctx := t.Context()
	admin := access.LocalAdmin()

	first, err := blobsStore.Put(ctx, admin, "", "text/plain", strings.NewReader("hello"))
	if err != nil {
		t.Fatalf("putting a blob: %v", err)
	}
	if len(first.ID) != 64 {
		t.Fatalf("the id is %q, not a SHA-256 in hex", first.ID)
	}
	if first.Size != 5 {
		t.Errorf("the blob is %d bytes", first.Size)
	}

	// The same bytes are the same blob.
	second, err := blobsStore.Put(ctx, admin, "", "text/plain", strings.NewReader("hello"))
	if err != nil {
		t.Fatalf("putting the same blob again: %v", err)
	}
	if second.ID != first.ID {
		t.Fatalf("the same bytes made %s and %s", first.ID, second.ID)
	}

	reader, found, err := blobsStore.Open(ctx, admin, first.ID)
	if err != nil {
		t.Fatalf("opening it: %v", err)
	}
	defer blobs.Close(reader)
	content, err := io.ReadAll(reader)
	if err != nil {
		t.Fatalf("reading it: %v", err)
	}
	if string(content) != "hello" {
		t.Fatalf("the blob is %q", content)
	}
	if found.Size != 5 {
		t.Errorf("the metadata says %d bytes", found.Size)
	}

	if err := blobsStore.Delete(ctx, admin, first.ID); err != nil {
		t.Fatalf("deleting it: %v", err)
	}
	if _, err := blobsStore.ByID(ctx, first.ID); !errors.Is(err, access.ErrNotFound) {
		t.Fatalf("a deleted blob is still there: %v", err)
	}
}

func TestAnotherUserCannotReadABlobTheyCannotSee(t *testing.T) {
	t.Parallel()

	opened, blobsStore := newBlobs(t)
	ctx := t.Context()
	checker := access.NewChecker(opened)

	// Alice owns a blob that belongs to the space; bob has nothing on the space.
	alice := access.Principal{Kind: access.KindUser, ID: "01halice", Email: "alice@example.test"}
	bob := access.Principal{Kind: access.KindUser, ID: "01hbob", Email: "bob@example.test"}
	for _, principal := range []access.Principal{alice, bob} {
		if _, err := opened.Queries().CreateUser(ctx, storedb.CreateUserParams{ID: principal.ID, Email: principal.Email}); err != nil {
			t.Fatalf("making %s: %v", principal.Email, err)
		}
	}
	if _, err := checker.Grant(ctx, access.SpaceURI("01hspace"), access.UserPrincipal(alice.ID), access.Manage); err != nil {
		t.Fatalf("granting alice the space: %v", err)
	}

	blob, err := blobsStore.Put(ctx, alice, access.SpaceURI("01hspace"), "text/plain", strings.NewReader("team notes"))
	if err != nil {
		t.Fatalf("putting a blob: %v", err)
	}

	if _, err := blobsStore.Put(ctx, bob, access.SpaceURI("01hspace"), "text/plain", strings.NewReader("nope")); err == nil {
		t.Fatal("bob put a blob in a space he may not use")
	}
	if _, _, err := blobsStore.Open(ctx, bob, blob.ID); !errors.Is(err, access.ErrDenied) {
		t.Fatalf("bob opened alice's blob: %v", err)
	}
	if _, _, err := blobsStore.Open(ctx, alice, blob.ID); err != nil {
		t.Fatalf("alice could not open her own blob: %v", err)
	}
}

func TestTheBlobRouteServesRanges(t *testing.T) {
	t.Parallel()

	_, blobsStore := newBlobs(t)
	ctx := t.Context()

	blob, err := blobsStore.Put(ctx, access.LocalAdmin(), "", "text/plain", strings.NewReader("0123456789"))
	if err != nil {
		t.Fatalf("putting a blob: %v", err)
	}

	handler := blobsStore.Handler()
	request := httptest.NewRequestWithContext(access.WithPrincipal(ctx, access.LocalAdmin()), http.MethodGet, "/blobs/"+blob.ID, nil)
	request.Header.Set("Range", "bytes=2-5")
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, request)

	response := recorder.Result()
	defer func() { _ = response.Body.Close() }()
	if response.StatusCode != http.StatusPartialContent {
		t.Fatalf("a range request answered %d", response.StatusCode)
	}
	body, err := io.ReadAll(response.Body)
	if err != nil {
		t.Fatalf("reading the answer: %v", err)
	}
	if !bytes.Equal(body, []byte("2345")) {
		t.Fatalf("the range is %q", body)
	}
}

func TestACollectedBlobIsGone(t *testing.T) {
	t.Parallel()

	opened, blobsStore := newBlobs(t)
	ctx := t.Context()

	blob, err := blobsStore.Put(ctx, access.LocalAdmin(), "", "text/plain", strings.NewReader("temporary"))
	if err != nil {
		t.Fatalf("putting a blob: %v", err)
	}
	// Nothing referencing it, and older than nothing.
	if _, err := opened.Pool().Exec(ctx, `UPDATE rookery.blobs SET created_at = now() - interval '2 days'`); err != nil {
		t.Fatalf("aging the blob: %v", err)
	}
	removed, err := blobsStore.Collect(ctx, 24*60*60*1e9)
	if err != nil {
		t.Fatalf("collecting: %v", err)
	}
	if removed != 1 {
		t.Fatalf("the collection removed %d blobs, want 1", removed)
	}
	if _, err := blobsStore.ByID(ctx, blob.ID); !errors.Is(err, access.ErrNotFound) {
		t.Fatalf("the collected blob is still there: %v", err)
	}
}
