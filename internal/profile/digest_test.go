package profile_test

import (
	"testing"

	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/profile"
)

// managed is a profile with one feature of one plugin, which most cases share.
func managed(t *testing.T) profile.Profile {
	t.Helper()
	vars, err := cborx.Marshal(map[string]any{"base_ssh_permit_root_login": "no"})
	if err != nil {
		t.Fatalf("encoding the profile's variables: %v", err)
	}
	return profile.Profile{
		Type:     profile.TypeManaged,
		Address:  "192.0.2.10",
		SSHUser:  "ops",
		Operator: "ops",
		Tailnet:  true,
		Vars:     vars,
		Features: []profile.Feature{
			{Plugin: "example", Feature: "hello"},
			{Plugin: "docker", Feature: "engine"},
		},
	}
}

func TestDigestIsStable(t *testing.T) {
	t.Parallel()

	plugins := map[string]string{"example": "0.1.0", "docker": "0.2.0", "incus": "0.3.0"}

	first, err := profile.Digest(managed(t), plugins, "1.0.0")
	if err != nil {
		t.Fatalf("Digest() = %v", err)
	}
	second, err := profile.Digest(managed(t), plugins, "1.0.0")
	if err != nil {
		t.Fatalf("Digest() = %v", err)
	}
	if first != second {
		t.Errorf("two digests of one profile differ: %s and %s", first, second)
	}
	if len(first) != 64 {
		t.Errorf("digest %q is not a SHA-256 in hex", first)
	}
}

func TestDigestIgnoresDefaultsAndUnusedPlugins(t *testing.T) {
	t.Parallel()

	stated := managed(t)
	stated.Port = profile.DefaultPort
	plugins := map[string]string{"example": "0.1.0", "docker": "0.2.0"}

	withDefaults, err := profile.Digest(stated, plugins, "1.0.0")
	if err != nil {
		t.Fatalf("Digest() = %v", err)
	}
	withoutDefaults, err := profile.Digest(managed(t), plugins, "1.0.0")
	if err != nil {
		t.Fatalf("Digest() = %v", err)
	}
	if withDefaults != withoutDefaults {
		t.Errorf("a stated default changed the digest")

	}
	// Installing a plugin no host uses must not make hosts behind.
	more, err := profile.Digest(managed(t), map[string]string{
		"example": "0.1.0", "docker": "0.2.0", "incus": "0.3.0",
	}, "1.0.0")
	if err != nil {
		t.Fatalf("Digest() = %v", err)
	}
	if more != withoutDefaults {
		t.Errorf("a plugin the profile does not name changed the digest")
	}
}

func TestDigestChanges(t *testing.T) {
	t.Parallel()

	plugins := map[string]string{"example": "0.1.0", "docker": "0.2.0"}
	base, err := profile.Digest(managed(t), plugins, "1.0.0")
	if err != nil {
		t.Fatalf("Digest() = %v", err)
	}

	cases := []struct {
		name    string
		profile func(t *testing.T) profile.Profile
		plugins map[string]string
		rookery string
	}{
		{
			name:    "another address",
			profile: func(t *testing.T) profile.Profile { p := managed(t); p.Address = "192.0.2.11"; return p },
			plugins: plugins,
			rookery: "1.0.0",
		},
		{
			name:    "another plugin version",
			profile: managed,
			plugins: map[string]string{"example": "0.1.1", "docker": "0.2.0"},
			rookery: "1.0.0",
		},
		{
			name:    "another rookery",
			profile: managed,
			plugins: plugins,
			rookery: "1.0.1",
		},
		{
			name: "features reordered",
			profile: func(t *testing.T) profile.Profile {
				p := managed(t)
				p.Features[0], p.Features[1] = p.Features[1], p.Features[0]
				return p
			},
			plugins: plugins,
			rookery: "1.0.0",
		},
		{
			name: "a feature dropped",
			profile: func(t *testing.T) profile.Profile {
				p := managed(t)
				p.Features = p.Features[:1]
				return p
			},
			plugins: plugins,
			rookery: "1.0.0",
		},
	}

	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			got, err := profile.Digest(c.profile(t), c.plugins, c.rookery)
			if err != nil {
				t.Fatalf("Digest() = %v", err)
			}
			if got == base {
				t.Errorf("the digest did not change")
			}
		})
	}
}

func TestDigestRefusesInvalidProfile(t *testing.T) {
	t.Parallel()

	if _, err := profile.Digest(profile.Profile{Type: profile.TypeManaged}, nil, "1.0.0"); err == nil {
		t.Fatal("Digest() accepted a profile without an address")
	}
}
