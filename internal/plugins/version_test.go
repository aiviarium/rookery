package plugins_test

import (
	"errors"
	"testing"

	"github.com/aiviarium/rookery/internal/plugins"
)

func TestParseVersion(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name    string
		version string
		want    string
		wantErr bool
	}{
		{"release", "1.2.3", "1.2.3", false},
		{"with a v", "v1.2.3", "1.2.3", false},
		{"pre-release", "1.2.3-rc.1", "1.2.3-rc.1", false},
		{"build", "1.2.3+build.5", "1.2.3+build.5", false},
		{"pre-release and build", "1.2.3-beta+2024", "1.2.3-beta+2024", false},
		{"partial", "1.2", "1.2.0", false},
		{"only major", "2", "2.0.0", false},
		{"leading zero", "01.2.3", "", true},
		{"four parts", "1.2.3.4", "", true},
		{"not a number", "1.x.3", "", true},
		{"empty", "", "", true},
		{"empty part", "1..3", "", true},
		{"bad pre-release", "1.2.3-a b", "", true},
	}

	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			version, err := plugins.ParseVersion(c.version)
			if c.wantErr {
				if !errors.Is(err, plugins.ErrInvalid) {
					t.Fatalf("ParseVersion(%q) = %v, want a refusal", c.version, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("ParseVersion(%q) = %v", c.version, err)
			}
			if got := version.String(); got != c.want {
				t.Errorf("ParseVersion(%q) = %q, want %q", c.version, got, c.want)
			}
		})
	}
}

func TestVersionCompare(t *testing.T) {
	t.Parallel()

	order := []string{
		"0.0.1",
		"0.1.0",
		"0.1.1",
		"0.2.0",
		"1.0.0-alpha",
		"1.0.0-alpha.1",
		"1.0.0-alpha.beta",
		"1.0.0-beta",
		"1.0.0-beta.2",
		"1.0.0-beta.11",
		"1.0.0-rc.1",
		"1.0.0",
		"1.2.3",
		"2.0.0",
	}
	for i := 1; i < len(order); i++ {
		older, newer := plugins.MustParseVersion(order[i-1]), plugins.MustParseVersion(order[i])
		if got := older.Compare(newer); got != -1 {
			t.Errorf("%s.Compare(%s) = %d, want -1", older, newer, got)
		}
		if got := newer.Compare(older); got != 1 {
			t.Errorf("%s.Compare(%s) = %d, want 1", newer, older, got)
		}
		if got := newer.Compare(newer); got != 0 {
			t.Errorf("%s.Compare(itself) = %d, want 0", newer, got)
		}
	}

	// Build metadata takes no part in the order, as semver says.
	withBuild := plugins.MustParseVersion("1.2.3+build.7")
	if got := withBuild.Compare(plugins.MustParseVersion("1.2.3")); got != 0 {
		t.Errorf("build metadata changed the order: %d", got)
	}
}

func TestRequirement(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name        string
		requirement string
		version     string
		want        bool
		wantErr     bool
	}{
		{name: "at least", requirement: ">=0.1.0", version: "0.1.0", want: true},
		{name: "at least, below", requirement: ">=0.1.0", version: "0.0.9", want: false},
		{name: "newer", requirement: ">1.0.0", version: "1.0.0", want: false},
		{name: "newer, yes", requirement: ">1.0.0", version: "1.0.1", want: true},
		{name: "below", requirement: "<2.0.0", version: "1.9.9", want: true},
		{name: "below, no", requirement: "<2.0.0", version: "2.0.0", want: false},
		{name: "caret", requirement: "^1.2", version: "1.9.0", want: true},
		{name: "caret, above", requirement: "^1.2", version: "2.0.0", want: false},
		{name: "caret, below", requirement: "^1.2", version: "1.1.0", want: false},
		{name: "caret on 0.x", requirement: "^0.2.3", version: "0.2.9", want: true},
		{name: "caret on 0.x, above", requirement: "^0.2.3", version: "0.3.0", want: false},
		{name: "caret on 0.0.x", requirement: "^0.0.3", version: "0.0.4", want: false},
		{name: "tilde", requirement: "~1.2.3", version: "1.2.9", want: true},
		{name: "tilde, above", requirement: "~1.2.3", version: "1.3.0", want: false},
		{name: "tilde on a major", requirement: "~1", version: "1.9.0", want: true},
		{name: "tilde on a major, above", requirement: "~1", version: "2.0.0", want: false},
		{name: "exact", requirement: "=1.2.3", version: "1.2.3", want: true},
		{name: "exact, no", requirement: "=1.2.3", version: "1.2.4", want: false},
		{name: "two constraints", requirement: ">=1.2.0, <1.3.0", version: "1.2.5", want: true},
		{name: "two constraints, out", requirement: ">=1.2.0 <1.3.0", version: "1.3.0", want: false},
		{name: "empty allows everything", requirement: "", version: "9.9.9", want: true},
		{name: "nonsense", requirement: "later", wantErr: true},
	}

	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			requirement, err := plugins.ParseRequirement(c.requirement)
			if c.wantErr {
				if !errors.Is(err, plugins.ErrInvalid) {
					t.Fatalf("ParseRequirement(%q) = %v, want a refusal", c.requirement, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("ParseRequirement(%q) = %v", c.requirement, err)
			}
			version := plugins.MustParseVersion(c.version)
			if got := requirement.Allows(version); got != c.want {
				t.Errorf("%q allows %s = %v, want %v", c.requirement, version, got, c.want)
			}
		})
	}
}

func TestRequirementString(t *testing.T) {
	t.Parallel()

	requirement, err := plugins.ParseRequirement(">=1.2.0, <2.0.0")
	if err != nil {
		t.Fatalf("ParseRequirement() = %v", err)
	}
	if got, want := requirement.String(), ">=1.2.0, <2.0.0"; got != want {
		t.Errorf("String() = %q, want %q", got, want)
	}
}
