package config_test

import (
	"bytes"
	"context"
	"errors"
	"testing"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/config"
	"github.com/aiviarium/rookery/internal/seal"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// alice is a user who may manage the space in these tests.
func alice(ctx context.Context, t *testing.T, opened *store.Store, checker *access.Checker) access.Principal {
	t.Helper()

	user, err := opened.Queries().CreateUser(ctx, store.CreateUserParams{ID: "01halice", Email: "alice@example.test"})
	if err != nil {
		t.Fatalf("making alice: %v", err)
	}
	if _, err := checker.Grant(ctx, access.SpaceURI("01hspace"), access.UserPrincipal(user.ID), access.Manage); err != nil {
		t.Fatalf("granting alice the space: %v", err)
	}
	return access.Principal{Kind: access.KindUser, ID: user.ID, Email: user.Email}
}

func TestASettingResolvesNearestFirst(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	checker := access.NewChecker(opened)
	ctx := t.Context()
	principal := alice(ctx, t, opened, checker)
	settings := config.NewSettings(opened)

	// The default, before anybody sets anything.
	value, err := settings.Get(ctx, "rookery/instance_name", principal, "")
	if err != nil {
		t.Fatalf("reading the default: %v", err)
	}
	if value.Scope != "default" || settingString(t, value.Value) != "Rookery" {
		t.Fatalf("the default is %q from %q", value.Value, value.Scope)
	}

	if err := settings.Set(ctx, "rookery/instance_name", config.ScopeGlobal, "", mustCBOR(t, "The Lab"), access.LocalAdmin(), checker); err != nil {
		t.Fatalf("setting it globally: %v", err)
	}
	value, err = settings.Get(ctx, "rookery/instance_name", principal, "")
	if err != nil {
		t.Fatalf("reading it: %v", err)
	}
	if value.Scope != "global" || settingString(t, value.Value) != "The Lab" {
		t.Fatalf("the value is %q from %q", value.Value, value.Scope)
	}

	// The timezone has a user scope: alice's own wins over the global one.
	if err := settings.Set(ctx, "rookery/timezone", config.ScopeGlobal, "", mustCBOR(t, "UTC"), access.LocalAdmin(), checker); err != nil {
		t.Fatalf("setting the timezone: %v", err)
	}
	if err := settings.Set(ctx, "rookery/timezone", config.ScopeUser, "", mustCBOR(t, "Europe/Berlin"), principal, checker); err != nil {
		t.Fatalf("setting alice's timezone: %v", err)
	}
	value, err = settings.Get(ctx, "rookery/timezone", principal, "")
	if err != nil {
		t.Fatalf("reading the timezone: %v", err)
	}
	if value.Scope != "user" || settingString(t, value.Value) != "Europe/Berlin" {
		t.Fatalf("the timezone is %q from %q", value.Value, value.Scope)
	}

	// Reset takes the user's value away, and the global one resolves again.
	if err := settings.Reset(ctx, "rookery/timezone", config.ScopeUser, "", principal, checker); err != nil {
		t.Fatalf("resetting: %v", err)
	}
	value, err = settings.Get(ctx, "rookery/timezone", principal, "")
	if err != nil {
		t.Fatalf("reading the timezone: %v", err)
	}
	if value.Scope != "global" {
		t.Fatalf("the timezone comes from %q", value.Scope)
	}
}

func TestOnlyAnAdminSetsGlobal(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	checker := access.NewChecker(opened)
	ctx := t.Context()
	principal := alice(ctx, t, opened, checker)
	settings := config.NewSettings(opened)

	if err := settings.Set(ctx, "rookery/instance_name", config.ScopeGlobal, "", mustCBOR(t, "Mine"), principal, checker); !errors.Is(err, access.ErrDenied) {
		t.Fatalf("a user who is not an admin set a global setting: %v", err)
	}
}

func TestSettingsRefuseWhatTheyDoNotHave(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	checker := access.NewChecker(opened)
	ctx := t.Context()
	principal := alice(ctx, t, opened, checker)
	settings := config.NewSettings(opened)

	if _, err := settings.Get(ctx, "nobody/knows", principal, ""); !errors.Is(err, access.ErrNotFound) {
		t.Fatalf("an unknown setting gave %v", err)
	}
	if err := settings.Set(ctx, "rookery/timezone", "planet", "", mustCBOR(t, "Mars"), principal, checker); !errors.Is(err, access.ErrInvalid) {
		t.Fatalf("an unknown scope gave %v", err)
	}
	// instance_name is a global setting alone.
	if err := settings.Set(ctx, "rookery/instance_name", config.ScopeUser, "", mustCBOR(t, "Mine"), principal, checker); !errors.Is(err, access.ErrInvalid) {
		t.Fatalf("a scope the setting has not gave %v", err)
	}
	// A setting's value has its type.
	if err := settings.Set(ctx, "rookery/notifications", config.ScopeGlobal, "", mustCBOR(t, "yes"), access.LocalAdmin(), checker); !errors.Is(err, access.ErrInvalid) {
		t.Fatalf("a string in a bool setting gave %v", err)
	}
}

func TestSecretsSealAndRekey(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	ctx := t.Context()

	first, err := seal.New()
	if err != nil {
		t.Fatalf("making a key: %v", err)
	}
	ring, err := seal.NewRing(first.ID(), first)
	if err != nil {
		t.Fatalf("making the ring: %v", err)
	}
	secrets := config.NewSecrets(opened, ring)

	if _, err := secrets.Set(ctx, "example_token", config.SecretScopeGlobal, []byte("hunter2")); err != nil {
		t.Fatalf("setting a secret: %v", err)
	}
	value, err := secrets.Value(ctx, "example_token", config.SecretScopeGlobal)
	if err != nil {
		t.Fatalf("reading it: %v", err)
	}
	if string(value) != "hunter2" {
		t.Fatalf("the value is %q", value)
	}

	// The value is sealed in the database.
	record, err := opened.Queries().GetSecret(ctx, store.GetSecretParams{Name: "example_token", Scope: config.SecretScopeGlobal})
	if err != nil {
		t.Fatalf("reading the row: %v", err)
	}
	if string(record.Value) == "hunter2" {
		t.Fatal("the secret is stored in the clear")
	}
	if record.KeyID != first.ID() {
		t.Errorf("the row's key is %s, want %s", record.KeyID, first.ID())
	}

	// Renaming reseals under the new name, so the value still opens.
	renamed, err := secrets.Rename(ctx, "example_token", config.SecretScopeGlobal, "example_second")
	if err != nil {
		t.Fatalf("renaming: %v", err)
	}
	if renamed.Name != "example_second" {
		t.Fatalf("the renamed secret is %q", renamed.Name)
	}
	value, err = secrets.Value(ctx, "example_second", config.SecretScopeGlobal)
	if err != nil {
		t.Fatalf("reading the renamed secret: %v", err)
	}
	if string(value) != "hunter2" {
		t.Fatalf("the renamed value is %q", value)
	}

	// A second key reseals everything on a rekey.
	second, err := seal.New()
	if err != nil {
		t.Fatalf("making a second key: %v", err)
	}
	rekeyRing, err := seal.NewRing(second.ID(), first, second)
	if err != nil {
		t.Fatalf("making the second ring: %v", err)
	}
	rekeyed := config.NewSecrets(opened, rekeyRing)
	rows, err := rekeyed.Rekey(ctx)
	if err != nil {
		t.Fatalf("rekeying: %v", err)
	}
	if rows != 1 {
		t.Errorf("the rekey resealed %d rows, want 1", rows)
	}
	record, err = opened.Queries().GetSecret(ctx, store.GetSecretParams{Name: "example_second", Scope: config.SecretScopeGlobal})
	if err != nil {
		t.Fatalf("reading the row: %v", err)
	}
	if record.KeyID != second.ID() {
		t.Errorf("after the rekey the row's key is %s, want %s", record.KeyID, second.ID())
	}
}

func TestSecretsRefuseAnotherScope(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	ctx := t.Context()
	secrets := config.NewSecrets(opened, newRing(t))

	for _, scope := range []string{"planet", "plugin/", "host/", ""} {
		if _, err := secrets.Set(ctx, "one", scope, []byte("x")); !errors.Is(err, access.ErrInvalid) {
			t.Errorf("the scope %q was accepted: %v", scope, err)
		}
	}
	for _, scope := range []string{config.SecretScopeGlobal, "plugin/example", "provisioner/lab", "host/forge"} {
		if _, err := secrets.Set(ctx, "one", scope, []byte("x")); err != nil {
			t.Errorf("the scope %q was refused: %v", scope, err)
		}
	}
}

func TestConnectionsSealTheirFields(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	ctx := t.Context()
	connections := config.NewConnections(opened, newRing(t))

	connection, err := connections.Create(ctx, "registry", "lab-registry", map[string]string{
		"url":      "https://registry.example.test",
		"username": "puller",
		"password": "hunter2",
	})
	if err != nil {
		t.Fatalf("creating a connection: %v", err)
	}
	if connection.Name != "lab-registry" {
		t.Fatalf("the connection is %q", connection.Name)
	}

	values, err := connections.Values(ctx, connection.ID)
	if err != nil {
		t.Fatalf("reading its values: %v", err)
	}
	if values["password"] != "hunter2" {
		t.Fatalf("the password is %q", values["password"])
	}

	// The row holds the fields sealed.
	record, err := opened.Queries().GetConnection(ctx, connection.ID)
	if err != nil {
		t.Fatalf("reading the row: %v", err)
	}
	if len(record.Values) == 0 || bytes.Contains(record.Values, []byte("hunter2")) {
		t.Fatal("the connection's password is stored in the clear")
	}

	// An update keeps a field it leaves out.
	renamed := "renamed-registry"
	updated, err := connections.Update(ctx, connection.ID, &renamed, map[string]string{"url": "https://other.example.test"})
	if err != nil {
		t.Fatalf("updating: %v", err)
	}
	if updated.Name != "renamed-registry" {
		t.Fatalf("the connection is %q", updated.Name)
	}
	values, err = connections.Values(ctx, connection.ID)
	if err != nil {
		t.Fatalf("reading its values: %v", err)
	}
	if values["password"] != "hunter2" || values["url"] != "https://other.example.test" {
		t.Fatalf("the values are %+v", values)
	}

	// A required field is required.
	if _, err := connections.Create(ctx, "registry", "half", map[string]string{"url": "https://x.example.test"}); !errors.Is(err, access.ErrInvalid) {
		t.Fatalf("a connection without its password was made: %v", err)
	}
}

// mustCBOR encodes a setting's value.
func mustCBOR(t *testing.T, value any) []byte {
	t.Helper()

	encoded, err := cborx.Marshal(value)
	if err != nil {
		t.Fatalf("encoding %v: %v", value, err)
	}
	return encoded
}

// settingString reads a setting's string value.
func settingString(t *testing.T, value []byte) string {
	t.Helper()

	var text string
	if err := cborx.Unmarshal(value, &text); err != nil {
		t.Fatalf("reading %q: %v", value, err)
	}
	return text
}

// newRing is a ring with one new key.
func newRing(t *testing.T) *seal.Ring {
	t.Helper()

	key, err := seal.New()
	if err != nil {
		t.Fatalf("making a key: %v", err)
	}
	ring, err := seal.NewRing(key.ID(), key)
	if err != nil {
		t.Fatalf("making the ring: %v", err)
	}
	return ring
}
