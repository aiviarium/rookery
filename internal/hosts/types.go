// Package hosts holds the machines Rookery runs: their records, profiles,
// labels and types, and the capability check every service asks before it does
// something a host may not be able to do.
//
// The type table is data: a type is what a host can do now, from ssh, reached
// only over SSH, to managed, which has rookeryd, box and the tailnet, to
// workstation, which has the desktop as well. A host's type only grows, and an
// apply is what makes it grow.
package hosts

import (
	"fmt"
	"sort"
	"strings"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/profile"
)

// Capability is what a call needs a host to be able to do: one row of the type
// table.
type Capability string

// The capabilities the type table knows.
const (
	// CapabilityContact is first contact: logging in over SSH with the ring's
	// key or a credential, and installing the key.
	CapabilityContact Capability = "contact"
	// CapabilityApply is running Ansible against the host.
	CapabilityApply Capability = "apply"
	// CapabilityExec is running a command over SSH or through rookeryd.
	CapabilityExec Capability = "exec"
	// CapabilityTerminal is a PTY session on the host.
	CapabilityTerminal Capability = "terminal"
	// CapabilityFiles is reading and writing the host's files.
	CapabilityFiles Capability = "files"
	// CapabilityPorts is reaching a port on the host's loopback.
	CapabilityPorts Capability = "ports"
	// CapabilityDesktop is the host's shared desktop.
	CapabilityDesktop Capability = "desktop"
	// CapabilityChannel is the host's channel: topics and host services.
	CapabilityChannel Capability = "channel"
	// CapabilityActions is the actions the host's features declare.
	CapabilityActions Capability = "actions"
)

// capabilities is the type table: what each type can do. A type that can do one
// can do every one before it, but the table says each out so that a refusal can
// name what is missing.
var capabilities = map[profile.Type]map[Capability]bool{
	profile.TypeSSH: {
		CapabilityContact: true,
		CapabilityApply:   true,
		CapabilityExec:    true,
	},
	profile.TypeManaged: {
		CapabilityContact:  true,
		CapabilityApply:    true,
		CapabilityExec:     true,
		CapabilityTerminal: true,
		CapabilityFiles:    true,
		CapabilityPorts:    true,
		CapabilityChannel:  true,
		CapabilityActions:  true,
	},
	profile.TypeWorkstation: {
		CapabilityContact:  true,
		CapabilityApply:    true,
		CapabilityExec:     true,
		CapabilityTerminal: true,
		CapabilityFiles:    true,
		CapabilityPorts:    true,
		CapabilityChannel:  true,
		CapabilityActions:  true,
		CapabilityDesktop:  true,
	},
}

// All returns every capability, in a stable order.
func All() []Capability {
	seen := make(map[Capability]bool)
	var all []Capability
	for _, table := range capabilities {
		for capability := range table {
			if !seen[capability] {
				seen[capability] = true
				all = append(all, capability)
			}
		}
	}
	sort.Slice(all, func(i, j int) bool { return all[i] < all[j] })
	return all
}

// ParseCapability reads a capability by name.
func ParseCapability(text string) (Capability, error) {
	capability := Capability(strings.ToLower(strings.TrimSpace(text)))
	for _, known := range All() {
		if capability == known {
			return capability, nil
		}
	}
	return "", fmt.Errorf("%w: %q is not a capability: %s", access.ErrInvalid, text, join(All()))
}

// join writes capabilities for a message.
func join(values []Capability) string {
	words := make([]string, len(values))
	for i, value := range values {
		words[i] = string(value)
	}
	return strings.Join(words, ", ")
}

// Can reports whether a type can do a capability.
func Can(hostType profile.Type, capability Capability) bool {
	return capabilities[hostType][capability]
}

// Check refuses unless the host can do the capability. An unknown type, an ssh
// host asked for a desktop and a managed host asked for one all refuse, and the
// refusal says what would change the answer.
func Check(host Record, capability Capability) error {
	return CheckType(host.Name, host.Type, capability)
}

// CheckType is Check for a host that is only its name and type.
func CheckType(name string, hostType profile.Type, capability Capability) error {
	if Can(hostType, capability) {
		return nil
	}
	reason, ok := reasonFor(hostType, capability)
	if !ok {
		reason = fmt.Sprintf("a %s host cannot do this", hostType)
	}
	return &access.Denied{
		Resource:   access.HostURI(name),
		Capability: string(capability),
		Reason:     reason,
	}
}

// reasonFor says what would make a host of this type able to do the capability.
func reasonFor(hostType profile.Type, capability Capability) (string, bool) {
	switch capability {
	case CapabilityDesktop:
		switch hostType {
		case profile.TypeSSH, profile.TypeManaged:
			return "apply the base role, then make the host a workstation: give it a desktop in its profile and apply again", true
		}
	case CapabilityTerminal, CapabilityFiles, CapabilityPorts, CapabilityChannel, CapabilityActions:
		if hostType == profile.TypeSSH {
			return "apply the base role: it installs rookeryd, box and the tailnet, which makes the host managed", true
		}
	}
	return "", false
}

// Grow returns the type a host has after an apply of a profile of type want: a
// type only grows, so a profile that says less than the host now has changes
// nothing.
func Grow(now profile.Type, want profile.Type) profile.Type {
	if rank(want) > rank(now) {
		return want
	}
	return now
}

// rank orders the types from the least to the most.
func rank(t profile.Type) int {
	switch t {
	case profile.TypeSSH:
		return 1
	case profile.TypeManaged:
		return 2
	case profile.TypeWorkstation:
		return 3
	default:
		return 0
	}
}
