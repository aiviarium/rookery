package profile_test

import (
	"errors"
	"testing"

	"github.com/aiviarium/rookery/internal/profile"
)

func TestNormalizeFillsDefaults(t *testing.T) {
	t.Parallel()

	got := profile.Profile{Type: profile.TypeWorkstation}.Normalize()
	if got.Port != profile.DefaultPort {
		t.Errorf("port = %d, want %d", got.Port, profile.DefaultPort)
	}
	if got.SSHUser != profile.DefaultSSHUser || got.Operator != profile.DefaultOperator {
		t.Errorf("users = %q/%q, want %q/%q", got.SSHUser, got.Operator, profile.DefaultSSHUser, profile.DefaultOperator)
	}
	if got.Desktop != nil {
		t.Errorf("desktop = %+v, want none: the profile had none", got.Desktop)
	}
	if got.Vars == nil {
		t.Error("vars = nil, want an empty slice: the encoding must not depend on how a profile was built")
	}
}

func TestNormalizeDesktop(t *testing.T) {
	t.Parallel()

	got := profile.Profile{
		Type:    profile.TypeWorkstation,
		Desktop: &profile.Desktop{},
	}.Normalize()
	if got.Desktop.User != profile.DefaultDesktopUser {
		t.Errorf("desktop user = %q, want %q", got.Desktop.User, profile.DefaultDesktopUser)
	}
	if got.Desktop.Width != profile.DefaultWidth || got.Desktop.Height != profile.DefaultHeight {
		t.Errorf("screen = %dx%d, want %dx%d", got.Desktop.Width, got.Desktop.Height, profile.DefaultWidth, profile.DefaultHeight)
	}
}

func TestValidate(t *testing.T) {
	t.Parallel()

	valid := profile.Profile{
		Type:     profile.TypeManaged,
		Address:  "192.0.2.10",
		SSHUser:  "ops",
		Operator: "ops",
		Tailnet:  true,
	}
	workstation := valid
	workstation.Type = profile.TypeWorkstation
	workstation.Desktop = &profile.Desktop{User: "dev"}

	cases := []struct {
		name    string
		profile profile.Profile
		wantErr bool
	}{
		{"managed", valid, false},
		{"workstation", workstation, false},
		{"no type", profile.Profile{Address: "192.0.2.10"}, true},
		{"unknown type", profile.Profile{Type: "laptop", Address: "192.0.2.10"}, true},
		{"no address", profile.Profile{Type: profile.TypeManaged}, true},
		{"port out of range", profile.Profile{Type: profile.TypeManaged, Address: "192.0.2.10", Port: 70000}, true},
		{"ssh user with a space", profile.Profile{Type: profile.TypeManaged, Address: "192.0.2.10", SSHUser: "op s"}, true},
		{"workstation without a desktop", profile.Profile{Type: profile.TypeWorkstation, Address: "192.0.2.10"}, true},
		{"desktop on a managed host", profile.Profile{
			Type: profile.TypeManaged, Address: "192.0.2.10", Desktop: &profile.Desktop{User: "dev"},
		}, true},
		{"root operator on a managed host", profile.Profile{
			Type: profile.TypeManaged, Address: "192.0.2.10", Operator: "root",
		}, true},
		{"feature applied twice", profile.Profile{
			Type: profile.TypeManaged, Address: "192.0.2.10",
			Features: []profile.Feature{{Plugin: "example", Feature: "hello"}, {Plugin: "example", Feature: "hello"}},
		}, true},
		{"feature id is not an id", profile.Profile{
			Type: profile.TypeManaged, Address: "192.0.2.10",
			Features: []profile.Feature{{Plugin: "example", Feature: "Hello There"}},
		}, true},
	}

	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			err := c.profile.Normalize().Validate()
			if c.wantErr && !errors.Is(err, profile.ErrInvalid) {
				t.Fatalf("Validate() = %v, want an invalid profile", err)
			}
			if !c.wantErr && err != nil {
				t.Fatalf("Validate() = %v, want nil", err)
			}
		})
	}
}

func TestFeatureRole(t *testing.T) {
	t.Parallel()

	feature := profile.Feature{Plugin: "example", Feature: "hello"}
	if got, want := feature.Role(), "example__hello"; got != want {
		t.Errorf("Role() = %q, want %q", got, want)
	}
}

func TestParseType(t *testing.T) {
	t.Parallel()

	if got, err := profile.ParseType("workstation"); err != nil || got != profile.TypeWorkstation {
		t.Errorf("ParseType(workstation) = %q, %v", got, err)
	}
	if _, err := profile.ParseType("laptop"); !errors.Is(err, profile.ErrUnknownType) {
		t.Errorf("ParseType(laptop) error = %v, want ErrUnknownType", err)
	}
}
