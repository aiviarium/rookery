package hosts_test

import (
	"context"
	"errors"
	"testing"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/hosts"
	"github.com/aiviarium/rookery/internal/profile"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// add is a host to add in most cases.
func add(name string) hosts.AddInput {
	return hosts.AddInput{
		Name: name,
		Type: profile.TypeSSH,
		Profile: profile.Profile{
			Type:    profile.TypeSSH,
			Address: "192.0.2.10",
			SSHUser: "ops",
			Tailnet: true,
		},
		Labels: map[string]string{"role": "desk", "site": "lab"},
	}
}

func TestAddAndGet(t *testing.T) {
	t.Parallel()
	opened := storetest.New(t)
	hostStore := hosts.New(opened)
	ctx := context.Background()

	record, err := hostStore.Add(ctx, add("forge"))
	if err != nil {
		t.Fatalf("Add() = %v", err)
	}
	if record.Type != profile.TypeSSH || record.Profile.Address != "192.0.2.10" {
		t.Errorf("record = %+v, want an ssh host at 192.0.2.10", record)
	}
	if record.Labels["role"] != "desk" {
		t.Errorf("labels = %v, want role=desk", record.Labels)
	}

	got, err := hostStore.Get(ctx, "forge")
	if err != nil {
		t.Fatalf("Get() = %v", err)
	}
	if got.Profile.Port != profile.DefaultPort || got.Profile.Operator != profile.DefaultOperator {
		t.Errorf("profile = %+v, want the defaults filled in", got.Profile)
	}

	if _, err := hostStore.Get(ctx, "nope"); !errors.Is(err, access.ErrNotFound) {
		t.Errorf("Get(unknown) = %v, want NotFound", err)
	}
	if _, err := hostStore.Add(ctx, add("forge")); err == nil {
		t.Error("Add() of a name that is taken succeeded")
	}
}

func TestListFiltersLabels(t *testing.T) {
	t.Parallel()
	opened := storetest.New(t)
	hostStore := hosts.New(opened)
	ctx := context.Background()

	for _, name := range []string{"forge", "hearth", "kiln"} {
		if _, err := hostStore.Add(ctx, add(name)); err != nil {
			t.Fatalf("adding %s: %v", name, err)
		}
	}
	if err := hostStore.SetLabels(ctx, "kiln", map[string]string{"site": "home"}, nil); err != nil {
		t.Fatalf("SetLabels() = %v", err)
	}

	records, err := hostStore.List(ctx, hosts.Filter{Labels: map[string]string{"site": "lab"}})
	if err != nil {
		t.Fatalf("List() = %v", err)
	}
	if len(records) != 2 {
		t.Errorf("List(site=lab) = %d hosts, want 2", len(records))
	}
	records, err = hostStore.List(ctx, hosts.Filter{After: "forge", Limit: 1})
	if err != nil {
		t.Fatalf("List() = %v", err)
	}
	if len(records) != 1 || records[0].Name != "hearth" {
		t.Errorf("List(after forge, limit 1) = %v, want hearth", records)
	}
}

func TestSetProfileRecordsTheDesiredType(t *testing.T) {
	t.Parallel()
	opened := storetest.New(t)
	hostStore := hosts.New(opened)
	ctx := context.Background()

	if _, err := hostStore.Add(ctx, add("forge")); err != nil {
		t.Fatalf("Add() = %v", err)
	}
	managed := add("forge").Profile
	managed.Type = profile.TypeManaged
	record, err := hostStore.SetProfile(ctx, "forge", managed)
	if err != nil {
		t.Fatalf("SetProfile(managed) = %v", err)
	}
	if record.Type != profile.TypeSSH || record.Profile.Type != profile.TypeManaged {
		t.Errorf("type = %s, desired type = %s, want ssh and managed", record.Type, record.Profile.Type)
	}
	if _, err := hostStore.MarkApplied(ctx, "forge", "digest", profile.TypeManaged, ""); err != nil {
		t.Fatal(err)
	}

	// A profile that asks for less than the host has does not take the type
	// away: a host cannot unlearn rookeryd.
	back := add("forge").Profile
	back.Type = profile.TypeSSH
	record, err = hostStore.SetProfile(ctx, "forge", back)
	if err != nil {
		t.Fatalf("SetProfile(ssh) = %v", err)
	}
	if record.Type != profile.TypeManaged {
		t.Errorf("type = %s, want managed: a type only grows", record.Type)
	}

	bad := back
	bad.Address = ""
	if _, err := hostStore.SetProfile(ctx, "forge", bad); err == nil {
		t.Error("SetProfile() of a profile with no address succeeded")
	}
}

func TestMarkApplied(t *testing.T) {
	t.Parallel()
	opened := storetest.New(t)
	hostStore := hosts.New(opened)
	ctx := context.Background()

	if _, err := hostStore.Add(ctx, add("forge")); err != nil {
		t.Fatalf("Add() = %v", err)
	}
	record, err := hostStore.MarkApplied(ctx, "forge", "abc123", profile.TypeManaged, "100.64.0.5")
	if err != nil {
		t.Fatalf("MarkApplied() = %v", err)
	}
	if record.AppliedDigest != "abc123" || record.Type != profile.TypeManaged || record.Address != "100.64.0.5" {
		t.Errorf("record = %+v, want the applied digest, type and address", record)
	}
	if record.AppliedAt == nil || record.LastSeenAt == nil {
		t.Error("the apply recorded no times")
	}
	if record.Behind("abc123") {
		t.Error("a host just applied is behind")
	}
	if !record.Behind("def456") {
		t.Error("a host with another digest is not behind")
	}
}

func TestRenameKeepsGrantsAndRemoveDeletesThem(t *testing.T) {
	t.Parallel()
	opened := storetest.New(t)
	hostStore := hosts.New(opened)
	ctx := context.Background()

	record, err := hostStore.Add(ctx, add("forge"))
	if err != nil {
		t.Fatalf("Add() = %v", err)
	}
	// The host's resource and a grant on it, as the service makes them.
	if err := opened.Queries().UpsertResource(ctx, store.UpsertResourceParams{Uri: access.HostURI("forge")}); err != nil {
		t.Fatalf("creating the resource: %v", err)
	}
	if _, err := opened.Queries().UpsertGrant(ctx, store.UpsertGrantParams{
		Resource:  access.HostURI("forge"),
		Principal: access.UserPrincipal("user-1"),
		Level:     int16(access.Manage),
	}); err != nil {
		t.Fatalf("granting: %v", err)
	}
	_ = record

	renamed, err := hostStore.Rename(ctx, "forge", "anvil")
	if err != nil {
		t.Fatalf("Rename() = %v", err)
	}
	if renamed.Name != "anvil" {
		t.Errorf("name = %q, want anvil", renamed.Name)
	}
	grants, err := opened.Queries().ListGrantsForResource(ctx, access.HostURI("anvil"))
	if err != nil {
		t.Fatalf("listing the grants: %v", err)
	}
	if len(grants) != 1 {
		t.Errorf("the renamed host has %d grants, want 1", len(grants))
	}

	if err := hostStore.Remove(ctx, "anvil"); err != nil {
		t.Fatalf("Remove() = %v", err)
	}
	grants, err = opened.Queries().ListGrantsForResource(ctx, access.HostURI("anvil"))
	if err != nil {
		t.Fatalf("listing the grants after the removal: %v", err)
	}
	if len(grants) != 0 {
		t.Errorf("the removed host kept %d grants", len(grants))
	}
	if _, err := hostStore.Get(ctx, "anvil"); !errors.Is(err, access.ErrNotFound) {
		t.Errorf("Get() after Remove() = %v, want NotFound", err)
	}
}
