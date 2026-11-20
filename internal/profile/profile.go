// Package profile holds a host's profile: what kind of host it is, how Rookery
// reaches it, and what an apply puts on it. It also holds the applied digest,
// which says whether what a host runs is still what its profile asks for.
package profile

import (
	"errors"
	"fmt"
	"net/netip"
	"regexp"
	"slices"
)

// Type is what a host can do, from the least to the most.
type Type string

// The host types. A host's type only grows: an apply that installs the base
// role makes an ssh host a managed one, and the desktop role makes a managed
// host a workstation.
const (
	// TypeSSH is a host Rookery only reaches over SSH.
	TypeSSH Type = "ssh"
	// TypeManaged is a host with rookeryd, box and the tailnet.
	TypeManaged Type = "managed"
	// TypeWorkstation is a managed host with a desktop.
	TypeWorkstation Type = "workstation"
)

// ErrUnknownType is what ParseType returns for a type that is not one of the
// three.
var ErrUnknownType = errors.New("profile: unknown host type")

// ParseType reads a host type.
func ParseType(s string) (Type, error) {
	t := Type(s)
	if !t.Valid() {
		return "", fmt.Errorf("%w: %q, want ssh, managed or workstation", ErrUnknownType, s)
	}
	return t, nil
}

// Valid reports whether the type is one Rookery has.
func (t Type) Valid() bool {
	switch t {
	case TypeSSH, TypeManaged, TypeWorkstation:
		return true
	default:
		return false
	}
}

// Desktop is a workstation's desktop: one shared virtual session for one user.
type Desktop struct {
	// User is the desktop's user, "dev" when empty.
	User string `cbor:"user" json:"user"`
	// Width and Height are the screen's size in pixels, 1600 by 900 when zero.
	Width  int `cbor:"width" json:"width"`
	Height int `cbor:"height" json:"height"`
	// GPU gives the desktop user the render and video groups.
	GPU bool `cbor:"gpu" json:"gpu"`
}

// Feature is one feature of a plugin that a profile applies.
type Feature struct {
	// Plugin is the plugin's id, and Feature the feature's id in its manifest.
	Plugin  string `cbor:"plugin" json:"plugin"`
	Feature string `cbor:"feature" json:"feature"`
	// Vars are the feature's variables: a CBOR map, empty for none.
	Vars []byte `cbor:"vars" json:"vars"`
}

// Role is the Ansible role a feature is applied as, which is what its tasks are
// linked to in an apply's work directory.
func (f Feature) Role() string { return f.Plugin + "__" + f.Feature }

// Profile is what a host should be. It is stored as this structure's CBOR, and
// its digest is the applied digest.
type Profile struct {
	Type    Type   `cbor:"type" json:"type"`
	Address string `cbor:"address" json:"address"`
	// Port is the SSH port, 22 when zero.
	Port int `cbor:"port" json:"port"`
	// SSHUser is the account the server logs in as, "ops" when empty.
	SSHUser string `cbor:"ssh_user" json:"sshUser"`
	// Operator is the account an apply sets up, "ops" when empty.
	Operator string `cbor:"operator" json:"operator"`
	// Desktop is set on workstations and refused on other types.
	Desktop *Desktop `cbor:"desktop,omitempty" json:"desktop,omitempty"`
	// Tailnet says whether the host joins a tailnet.
	Tailnet bool `cbor:"tailnet" json:"tailnet"`
	// Registries are the names of the connection entries whose logins the host
	// gets.
	Registries []string `cbor:"registries,omitempty" json:"registries,omitempty"`
	// Features are the plugin features to apply, in the order they run.
	Features []Feature `cbor:"features,omitempty" json:"features,omitempty"`
	// Vars are the base role's variables: a CBOR map, empty for none.
	Vars []byte `cbor:"vars,omitempty" json:"vars,omitempty"`
}

// The defaults a profile takes when a field is left out.
const (
	DefaultPort        = 22
	DefaultSSHUser     = "ops"
	DefaultOperator    = "ops"
	DefaultDesktopUser = "dev"
	DefaultWidth       = 1600
	DefaultHeight      = 900
)

// ErrInvalid is the sentinel every validation error wraps.
var ErrInvalid = errors.New("profile: invalid")

var (
	featureID   = regexp.MustCompile(`^[a-z][a-z0-9]*(?:[-_][a-z0-9]+)*$`)
	hostAddress = regexp.MustCompile(`^[a-zA-Z0-9][a-zA-Z0-9._-]*$`)
	userName    = regexp.MustCompile(`^[a-z_][a-z0-9_-]*$`)
)

// Normalize fills in the defaults, so that two profiles that say the same thing
// have one encoding and one digest whatever way they were built.
func (p Profile) Normalize() Profile {
	if p.Port == 0 {
		p.Port = DefaultPort
	}
	if p.SSHUser == "" {
		p.SSHUser = DefaultSSHUser
	}
	if p.Operator == "" {
		p.Operator = DefaultOperator
	}
	if p.Desktop != nil {
		desktop := *p.Desktop
		if desktop.User == "" {
			desktop.User = DefaultDesktopUser
		}
		if desktop.Width == 0 {
			desktop.Width = DefaultWidth
		}
		if desktop.Height == 0 {
			desktop.Height = DefaultHeight
		}
		p.Desktop = &desktop
	}
	if p.Vars == nil {
		p.Vars = []byte{}
	}
	for i := range p.Features {
		if p.Features[i].Vars == nil {
			p.Features[i].Vars = []byte{}
		}
	}
	return p
}

// Validate checks a profile and says what is wrong with it. A profile it accepts
// is one an apply can run.
func (p Profile) Validate() error {
	if !p.Type.Valid() {
		return fmt.Errorf("%w: type %q, want ssh, managed or workstation", ErrInvalid, p.Type)
	}
	address, err := netip.ParseAddr(p.Address)
	if (err != nil && !hostAddress.MatchString(p.Address)) ||
		(address.Zone() != "" && !hostAddress.MatchString(address.Zone())) {
		return fmt.Errorf("%w: the profile needs a host name or IP address", ErrInvalid)
	}
	if p.Port < 0 || p.Port > 65535 {
		return fmt.Errorf("%w: port %d is not a port", ErrInvalid, p.Port)
	}
	if p.SSHUser != "" && !userName.MatchString(p.SSHUser) {
		return fmt.Errorf("%w: ssh user %q is not an account name", ErrInvalid, p.SSHUser)
	}
	if p.Operator != "" && !userName.MatchString(p.Operator) {
		return fmt.Errorf("%w: operator %q is not an account name", ErrInvalid, p.Operator)
	}
	if p.Operator == "root" && p.Type != TypeSSH {
		return fmt.Errorf("%w: a managed host's operator is not root", ErrInvalid)
	}
	switch p.Type {
	case TypeWorkstation:
		if p.Desktop == nil {
			return fmt.Errorf("%w: a workstation needs a desktop", ErrInvalid)
		}
	case TypeSSH, TypeManaged:
		if p.Desktop != nil {
			return fmt.Errorf("%w: a %s host has no desktop", ErrInvalid, p.Type)
		}
	}
	if p.Desktop != nil {
		if !userName.MatchString(p.Desktop.User) {
			return fmt.Errorf("%w: desktop user %q is not an account name", ErrInvalid, p.Desktop.User)
		}
		if p.Desktop.User == "root" || p.Desktop.User == p.Operator || p.Desktop.User == p.SSHUser {
			return fmt.Errorf("%w: the desktop user must be separate from the root, SSH and operator accounts", ErrInvalid)
		}
		if p.Desktop.Width < 0 || p.Desktop.Height < 0 {
			return fmt.Errorf("%w: the desktop's screen is %dx%d", ErrInvalid, p.Desktop.Width, p.Desktop.Height)
		}
	}

	seen := make(map[string]bool, len(p.Features))
	for _, feature := range p.Features {
		if !featureID.MatchString(feature.Plugin) {
			return fmt.Errorf("%w: plugin id %q is not an id", ErrInvalid, feature.Plugin)
		}
		if !featureID.MatchString(feature.Feature) {
			return fmt.Errorf("%w: feature id %q is not an id", ErrInvalid, feature.Feature)
		}
		if seen[feature.Role()] {
			return fmt.Errorf("%w: feature %s is applied twice", ErrInvalid, feature.Role())
		}
		seen[feature.Role()] = true
	}
	if slices.Contains(p.Registries, "") {
		return fmt.Errorf("%w: the profile names a registry connection without a name", ErrInvalid)
	}
	return nil
}
