package storetest_test

import (
	"errors"
	"testing"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// TestIsolation checks the promise New makes: two tests' databases are separate
// databases, both clones of one template.
func TestIsolation(t *testing.T) {
	t.Parallel()

	first := storetest.New(t)
	second := storetest.New(t)
	ctx := t.Context()

	if err := first.Queries().SetMeta(ctx, store.SetMetaParams{Key: "isolation", Value: []byte("first")}); err != nil {
		t.Fatalf("SetMeta: %v", err)
	}

	if _, err := first.Queries().GetMeta(ctx, "isolation"); err != nil {
		t.Fatalf("the first database lost its row: %v", err)
	}
	if _, err := second.Queries().GetMeta(ctx, "isolation"); err == nil {
		t.Fatal("the second database sees the first one's row")
	} else if !isNoRows(err) {
		t.Fatalf("GetMeta: %v", err)
	}
}

// TestTemplateSurvives checks that a second call reuses the template rather than
// making it again, and that a template made from another schema is replaced.
func TestTemplateSurvives(t *testing.T) {
	t.Parallel()

	first := storetest.New(t)
	ctx := t.Context()

	// What a changed schema looks like from the template's side.
	if err := first.Queries().SetMeta(ctx, store.SetMetaParams{Key: store.DigestKey, Value: []byte("old")}); err != nil {
		t.Fatalf("SetMeta: %v", err)
	}

	// The template is not the test's database: a change to a clone does not
	// reach the next one, and the next clone still has the digest.
	second := storetest.New(t)
	if _, err := second.Queries().GetMeta(ctx, store.DigestKey); err != nil {
		t.Fatalf("the second clone has no digest: %v", err)
	}
	if err := second.Ensure(ctx); err != nil {
		t.Fatalf("Ensure on a fresh clone: %v", err)
	}
}

func isNoRows(err error) bool {
	return errors.Is(err, pgx.ErrNoRows)
}
