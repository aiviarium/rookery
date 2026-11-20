package authn_test

import (
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/authn"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

func TestUsers(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	users := authn.NewUsers(opened)
	ctx := t.Context()

	first, err := users.Ensure(ctx, "Alice@Example.Test ", "Alice", false)
	if err != nil {
		t.Fatalf("ensuring alice: %v", err)
	}
	if first.Email != "alice@example.test" {
		t.Errorf("the email is stored as %q, want it lower-cased", first.Email)
	}

	again, err := users.Ensure(ctx, "alice@example.test", "Alice Again", false)
	if err != nil {
		t.Fatalf("ensuring alice again: %v", err)
	}
	if again.ID != first.ID {
		t.Errorf("a second sign-in made a second user: %s and %s", first.ID, again.ID)
	}

	cases := []struct {
		name  string
		email string
	}{
		{name: "empty", email: ""},
		{name: "no at", email: "alice"},
		{name: "no domain", email: "alice@"},
		{name: "no dot", email: "alice@example"},
	}
	for _, tc := range cases {
		t.Run("refuses "+tc.name, func(t *testing.T) {
			t.Parallel()
			if _, err := users.Ensure(ctx, tc.email, "", false); err == nil {
				t.Fatalf("Ensure(%q) was accepted", tc.email)
			}
		})
	}

	found, err := users.Search(ctx, "ali", "", 10)
	if err != nil {
		t.Fatalf("searching: %v", err)
	}
	if len(found) != 1 || found[0].ID != first.ID {
		t.Fatalf("Search found %d users", len(found))
	}
	if found, err := users.Search(ctx, "bob", "", 10); err != nil {
		t.Fatalf("searching for nobody: %v", err)
	} else if len(found) != 0 {
		t.Fatalf("Search found %d users for a name nobody has", len(found))
	}
}

func TestThereIsAlwaysAnAdmin(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	users := authn.NewUsers(opened)
	ctx := t.Context()

	admin, err := users.Ensure(ctx, "admin@example.test", "Admin", true)
	if err != nil {
		t.Fatalf("ensuring the admin: %v", err)
	}
	if _, err := users.SetFlags(ctx, admin.ID, false, false); err == nil {
		t.Fatal("the last admin gave the flag away")
	}
	if _, err := users.SetFlags(ctx, admin.ID, true, true); err == nil {
		t.Fatal("the last admin disabled itself")
	}
}

func TestSelfCannotDisableItself(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	users := authn.NewUsers(opened)
	ctx := t.Context()

	admin, err := users.Ensure(ctx, "admin@example.test", "Admin", true)
	if err != nil {
		t.Fatalf("ensuring the admin: %v", err)
	}
	other, err := users.Ensure(ctx, "other@example.test", "Other", true)
	if err != nil {
		t.Fatalf("ensuring the other admin: %v", err)
	}

	caller := access.WithPrincipal(ctx, access.Principal{Kind: access.KindUser, ID: other.ID, Admin: true})
	if _, err := users.SetFlags(caller, other.ID, true, true); err == nil {
		t.Fatal("an admin disabled itself")
	}
	if _, err := users.SetFlags(caller, admin.ID, true, true); err != nil {
		t.Fatalf("an admin disabled another admin: %v", err)
	}
}

func TestSessions(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	users := authn.NewUsers(opened)
	sessions := authn.NewSessions(opened)
	ctx := t.Context()

	user, err := users.Ensure(ctx, "alice@example.test", "Alice", false)
	if err != nil {
		t.Fatalf("ensuring alice: %v", err)
	}

	secret, created, err := sessions.Create(ctx, user.ID, "a browser")
	if err != nil {
		t.Fatalf("creating a session: %v", err)
	}
	if len(secret) < 40 {
		t.Errorf("a session secret is %d characters, want at least 40", len(secret))
	}
	if created.Csrf == "" {
		t.Error("the session has no CSRF token")
	}

	found, err := sessions.FromSecret(ctx, secret)
	if err != nil {
		t.Fatalf("reading the session back: %v", err)
	}
	if found.ID != created.ID || found.UserID != user.ID {
		t.Errorf("the session came back as %+v, want %+v", found, created)
	}

	if _, err := sessions.FromSecret(ctx, secret+"x"); !errors.Is(err, access.ErrUnauthenticated) {
		t.Fatalf("a wrong secret gave %v, want unauthenticated", err)
	}

	if err := sessions.Revoke(ctx, user.ID, created.ID); err != nil {
		t.Fatalf("revoking: %v", err)
	}
	if _, err := sessions.FromSecret(ctx, secret); !errors.Is(err, access.ErrUnauthenticated) {
		t.Fatalf("a revoked session gave %v, want unauthenticated", err)
	}
}

func TestAnotherUserCannotRevokeASession(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	users := authn.NewUsers(opened)
	sessions := authn.NewSessions(opened)
	ctx := t.Context()

	alice, _ := users.Ensure(ctx, "alice@example.test", "Alice", false)
	bob, _ := users.Ensure(ctx, "bob@example.test", "Bob", false)
	_, session, err := sessions.Create(ctx, alice.ID, "")
	if err != nil {
		t.Fatalf("creating a session: %v", err)
	}

	if err := sessions.Revoke(ctx, bob.ID, session.ID); !errors.Is(err, access.ErrNotFound) {
		t.Fatalf("bob revoked alice's session: %v", err)
	}
	if err := sessions.Revoke(ctx, alice.ID, session.ID); err != nil {
		t.Fatalf("alice could not revoke her own session: %v", err)
	}
}

func TestTokens(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	users := authn.NewUsers(opened)
	tokens := authn.NewTokens(opened)
	ctx := t.Context()

	user, _ := users.Ensure(ctx, "alice@example.test", "Alice", false)
	secret, created, err := tokens.Create(ctx, user.ID, "laptop")
	if err != nil {
		t.Fatalf("creating a token: %v", err)
	}
	if !strings.HasPrefix(secret, authn.TokenPrefix) {
		t.Errorf("the token %q does not start with %s", secret, authn.TokenPrefix)
	}

	found, err := tokens.Lookup(ctx, secret)
	if err != nil {
		t.Fatalf("looking the token up: %v", err)
	}
	if found.ID != created.ID {
		t.Errorf("the token came back as %s, want %s", found.ID, created.ID)
	}

	if _, err := tokens.Lookup(ctx, "rkpat_nonsense"); !errors.Is(err, access.ErrUnauthenticated) {
		t.Fatalf("a made-up token gave %v, want unauthenticated", err)
	}
	if _, err := tokens.Lookup(ctx, created.ID); !errors.Is(err, access.ErrUnauthenticated) {
		t.Fatalf("a token's id worked as a token: %v", err)
	}
	if err := tokens.Revoke(ctx, user.ID, created.ID); err != nil {
		t.Fatalf("revoking: %v", err)
	}
	if _, err := tokens.Lookup(ctx, secret); !errors.Is(err, access.ErrUnauthenticated) {
		t.Fatalf("a revoked token gave %v, want unauthenticated", err)
	}
}

func TestLinksWorkOnce(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	links := authn.NewLinks(opened, "https://rookery.example.test")
	ctx := t.Context()

	_, token, err := links.Create(ctx, "new@example.test", "")
	if err != nil {
		t.Fatalf("creating a link: %v", err)
	}
	if url := links.URL(token); !strings.HasPrefix(url, "https://rookery.example.test/auth/link/") {
		t.Errorf("the link's URL is %q", url)
	}

	link, err := links.Consume(ctx, token)
	if err != nil {
		t.Fatalf("using the link: %v", err)
	}
	if link.Email != "new@example.test" {
		t.Errorf("the link is for %q", link.Email)
	}
	if _, err := links.Consume(ctx, token); !errors.Is(err, access.ErrNotFound) {
		t.Fatalf("a spent link was used again: %v", err)
	}
}

func TestExpiredLinksAreRefused(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	links := authn.NewLinks(opened, "")
	ctx := t.Context()

	_, token, err := links.Create(ctx, "late@example.test", "")
	if err != nil {
		t.Fatalf("creating a link: %v", err)
	}
	if _, err := opened.Pool().Exec(ctx, `UPDATE rookery.links SET expires_at = $1`, time.Now().Add(-time.Minute)); err != nil {
		t.Fatalf("expiring the link: %v", err)
	}
	if _, err := links.Consume(ctx, token); !errors.Is(err, access.ErrNotFound) {
		t.Fatalf("an expired link was used: %v", err)
	}
}

func TestGroupsFromClaims(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	users := authn.NewUsers(opened)
	groups := authn.NewGroups(opened)
	ctx := t.Context()

	alice, _ := users.Ensure(ctx, "alice@example.test", "Alice", false)
	writers, err := groups.Create(ctx, "writers")
	if err != nil {
		t.Fatalf("creating a group: %v", err)
	}
	if err := groups.BindClaim(ctx, writers.ID, "groups", "writers"); err != nil {
		t.Fatalf("binding the claim: %v", err)
	}

	if err := groups.SyncClaims(ctx, alice.ID, map[string][]string{"groups": {"writers", "nobody"}}); err != nil {
		t.Fatalf("syncing claims: %v", err)
	}
	members, err := opened.Queries().ListGroupsForUser(ctx, alice.ID)
	if err != nil {
		t.Fatalf("reading the memberships: %v", err)
	}
	if len(members) != 1 || members[0].ID != writers.ID {
		t.Fatalf("alice is in %d groups, want the one her claim names", len(members))
	}

	// A sign-in whose claim no longer names the group takes her out of it, and a
	// membership added by hand stays.
	if err := groups.AddMember(ctx, writers.ID, "alice@example.test"); err != nil {
		t.Fatalf("adding alice by hand: %v", err)
	}
	if err := groups.SyncClaims(ctx, alice.ID, map[string][]string{"groups": nil}); err != nil {
		t.Fatalf("syncing claims: %v", err)
	}
	members, err = opened.Queries().ListGroupsForUser(ctx, alice.ID)
	if err != nil {
		t.Fatalf("reading the memberships: %v", err)
	}
	if len(members) != 1 {
		t.Fatalf("alice is in %d groups, want the one added by hand", len(members))
	}

	message, err := groups.Message(ctx, writers)
	if err != nil {
		t.Fatalf("reading the group: %v", err)
	}
	if len(message.GetMembers()) != 1 || len(message.GetClaims()) != 1 {
		t.Fatalf("the group has %d members and %d claims", len(message.GetMembers()), len(message.GetClaims()))
	}
}

func TestGroupNamesAreUnique(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	groups := authn.NewGroups(opened)
	ctx := t.Context()

	if _, err := groups.Create(ctx, "team"); err != nil {
		t.Fatalf("creating a group: %v", err)
	}
	if _, err := groups.Create(ctx, "team"); !errors.Is(err, access.ErrConflict) {
		t.Fatalf("a second group called team gave %v, want a conflict", err)
	}
}
