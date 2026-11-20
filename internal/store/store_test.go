package store_test

import (
	"errors"
	"strings"
	"testing"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

func TestEnsureIsIdempotent(t *testing.T) {
	t.Parallel()

	s := storetest.New(t)

	if err := s.Ensure(t.Context()); err != nil {
		t.Fatalf("Ensure on an applied database: %v", err)
	}

	recorded, err := s.Queries().GetMeta(t.Context(), store.DigestKey)
	if err != nil {
		t.Fatalf("GetMeta: %v", err)
	}
	if got := hexOf(recorded); got != s.Digest() {
		t.Errorf("the recorded digest is %s, want %s", got, s.Digest())
	}
}

func TestEnsureRefusesAnotherDigest(t *testing.T) {
	t.Parallel()

	s := storetest.New(t)
	ctx := t.Context()

	if err := s.Queries().SetMeta(ctx, store.SetMetaParams{
		Key:   store.DigestKey,
		Value: []byte("another schema's digest"),
	}); err != nil {
		t.Fatalf("SetMeta: %v", err)
	}

	err := s.Ensure(ctx)
	if !errors.Is(err, store.ErrDigestMismatch) {
		t.Fatalf("Ensure: got %v, want a digest mismatch", err)
	}

	var mismatch *store.DigestMismatchError
	if !errors.As(err, &mismatch) {
		t.Fatalf("Ensure: got %T, want *store.DigestMismatchError", err)
	}
	if got, want := mismatch.Recorded, hexOf([]byte("another schema's digest")); got != want {
		t.Errorf("recorded: got %s, want %s", got, want)
	}
	if mismatch.Current != s.Digest() {
		t.Errorf("current: got %s, want %s", mismatch.Current, s.Digest())
	}
	if !strings.Contains(err.Error(), "rookery server reset") {
		t.Errorf("the error does not say how to fix it: %v", err)
	}
}

func TestEnsureRefusesASchemaWithoutADigest(t *testing.T) {
	t.Parallel()

	s := storetest.New(t)
	ctx := t.Context()

	if _, err := s.Pool().Exec(ctx, `DELETE FROM rookery.meta`); err != nil {
		t.Fatalf("DELETE: %v", err)
	}

	err := s.Ensure(ctx)
	if !errors.Is(err, store.ErrDigestMismatch) {
		t.Fatalf("Ensure: got %v, want a digest mismatch", err)
	}
	var mismatch *store.DigestMismatchError
	if !errors.As(err, &mismatch) {
		t.Fatalf("Ensure: got %T, want *store.DigestMismatchError", err)
	}
	if mismatch.Recorded != "" {
		t.Errorf("recorded: got %s, want none", mismatch.Recorded)
	}
}

func TestReset(t *testing.T) {
	t.Parallel()

	s := storetest.New(t)
	ctx := t.Context()

	if err := s.Queries().SetMeta(ctx, store.SetMetaParams{Key: "a key", Value: []byte("a value")}); err != nil {
		t.Fatalf("SetMeta: %v", err)
	}
	if _, err := s.Pool().Exec(ctx, `CREATE TABLE rookery.extra (id int)`); err != nil {
		t.Fatalf("CREATE TABLE: %v", err)
	}

	if err := s.Reset(ctx); err != nil {
		t.Fatalf("Reset: %v", err)
	}

	if _, err := s.Queries().GetMeta(ctx, "a key"); !errors.Is(err, pgx.ErrNoRows) {
		t.Errorf("the row before the reset is still there: %v", err)
	}
	digest, err := s.Queries().GetMeta(ctx, store.DigestKey)
	if err != nil {
		t.Fatalf("GetMeta after Reset: %v", err)
	}
	if got := hexOf(digest); got != s.Digest() {
		t.Errorf("the digest after Reset is %s, want %s", got, s.Digest())
	}
	var exists bool
	if err := s.Pool().QueryRow(ctx, `SELECT to_regclass('rookery.extra') IS NOT NULL`).Scan(&exists); err != nil {
		t.Fatalf("to_regclass: %v", err)
	}
	if exists {
		t.Error("a table from before the reset is still there")
	}
}

func TestQueries(t *testing.T) {
	t.Parallel()

	s := storetest.New(t)
	ctx := t.Context()

	if err := s.Queries().SetMeta(ctx, store.SetMetaParams{Key: "k", Value: []byte{1, 2}}); err != nil {
		t.Fatalf("SetMeta: %v", err)
	}
	value, err := s.Queries().GetMeta(ctx, "k")
	if err != nil {
		t.Fatalf("GetMeta: %v", err)
	}
	if string(value) != "\x01\x02" {
		t.Errorf("GetMeta: got %v, want 01 02", value)
	}

	// SetMeta replaces a value, and its time.
	if err := s.Queries().SetMeta(ctx, store.SetMetaParams{Key: "k", Value: []byte{3}}); err != nil {
		t.Fatalf("SetMeta again: %v", err)
	}
	value, err = s.Queries().GetMeta(ctx, "k")
	if err != nil {
		t.Fatalf("GetMeta again: %v", err)
	}
	if string(value) != "\x03" {
		t.Errorf("GetMeta after SetMeta: got %v, want 03", value)
	}

	if _, err := s.Queries().GetMeta(ctx, "missing"); !errors.Is(err, pgx.ErrNoRows) {
		t.Errorf("GetMeta of a missing key: got %v, want pgx.ErrNoRows", err)
	}
}

func TestOpen(t *testing.T) {
	t.Parallel()

	s := storetest.New(t)

	if got := s.Pool().Config().MaxConns; got != 4 {
		t.Errorf("MaxConns: got %d, want 4", got)
	}
	if len(s.Digest()) != 64 {
		t.Errorf("Digest: got %q, want 64 hex digits", s.Digest())
	}
}

func hexOf(value []byte) string {
	const hexDigits = "0123456789abcdef"
	var out strings.Builder
	for _, b := range value {
		out.WriteByte(hexDigits[b>>4])
		out.WriteByte(hexDigits[b&0xf])
	}
	return out.String()
}
