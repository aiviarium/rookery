package hosts_test

import (
	"errors"
	"testing"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/hosts"
	"github.com/aiviarium/rookery/internal/profile"
)

func TestCheckType(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name       string
		hostType   profile.Type
		capability hosts.Capability
		allowed    bool
	}{
		{"an ssh host executes", profile.TypeSSH, hosts.CapabilityExec, true},
		{"an ssh host applies", profile.TypeSSH, hosts.CapabilityApply, true},
		{"an ssh host has no desktop", profile.TypeSSH, hosts.CapabilityDesktop, false},
		{"an ssh host has no terminals", profile.TypeSSH, hosts.CapabilityTerminal, false},
		{"a managed host has terminals", profile.TypeManaged, hosts.CapabilityTerminal, true},
		{"a managed host has ports", profile.TypeManaged, hosts.CapabilityPorts, true},
		{"a managed host has no desktop", profile.TypeManaged, hosts.CapabilityDesktop, false},
		{"a workstation has a desktop", profile.TypeWorkstation, hosts.CapabilityDesktop, true},
		{"a workstation has terminals", profile.TypeWorkstation, hosts.CapabilityTerminal, true},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			err := hosts.CheckType("forge", test.hostType, test.capability)
			if test.allowed && err != nil {
				t.Fatalf("CheckType() = %v, want allowed", err)
			}
			if test.allowed {
				return
			}
			var denied *access.Denied
			if !errors.As(err, &denied) {
				t.Fatalf("CheckType() = %v, want a refusal", err)
			}
			if denied.Capability != string(test.capability) {
				t.Errorf("refusal names %q, want %q", denied.Capability, test.capability)
			}
			if denied.Reason == "" {
				t.Error("the refusal says nothing about what would change the answer")
			}
		})
	}
}

func TestGrow(t *testing.T) {
	t.Parallel()

	if got := hosts.Grow(profile.TypeManaged, profile.TypeSSH); got != profile.TypeManaged {
		t.Errorf("Grow(managed, ssh) = %s, want managed: a type only grows", got)
	}
	if got := hosts.Grow(profile.TypeSSH, profile.TypeWorkstation); got != profile.TypeWorkstation {
		t.Errorf("Grow(ssh, workstation) = %s, want workstation", got)
	}
}

func TestParseCapability(t *testing.T) {
	t.Parallel()

	if _, err := hosts.ParseCapability("terminal"); err != nil {
		t.Errorf("ParseCapability(terminal) = %v", err)
	}
	if _, err := hosts.ParseCapability("teleport"); err == nil {
		t.Error("ParseCapability(teleport) succeeded")
	}
}
