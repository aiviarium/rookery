package hosts

import (
	"fmt"
	"sync"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/link"
	"github.com/aiviarium/rookery/internal/profile"
)

// Credentials holds the credential of a first contact between the call that
// took it and the job that uses it. Nothing about it is stored or logged: it
// lives in this map for as long as the job waits, and is taken once.
type Credentials struct {
	mu   sync.Mutex
	byID map[string]link.Credential
}

// NewCredentials returns an empty holder.
func NewCredentials() *Credentials {
	return &Credentials{byID: make(map[string]link.Credential)}
}

// Put keeps a credential for a job.
func (c *Credentials) Put(jobID string, credential link.Credential) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.byID[jobID] = credential
}

// Take returns a job's credential and forgets it.
func (c *Credentials) Take(jobID string) link.Credential {
	c.mu.Lock()
	defer c.mu.Unlock()
	credential := c.byID[jobID]
	delete(c.byID, jobID)
	return credential
}

// toProtoType is a host type as the API has it.
func toProtoType(t profile.Type) rookeryv1.HostType {
	switch t {
	case profile.TypeSSH:
		return rookeryv1.HostType_HOST_TYPE_SSH
	case profile.TypeManaged:
		return rookeryv1.HostType_HOST_TYPE_MANAGED
	case profile.TypeWorkstation:
		return rookeryv1.HostType_HOST_TYPE_WORKSTATION
	default:
		return rookeryv1.HostType_HOST_TYPE_UNSPECIFIED
	}
}

// fromProtoType is a host type from the API, ssh when it is not given.
func fromProtoType(t rookeryv1.HostType) profile.Type {
	switch t {
	case rookeryv1.HostType_HOST_TYPE_MANAGED:
		return profile.TypeManaged
	case rookeryv1.HostType_HOST_TYPE_WORKSTATION:
		return profile.TypeWorkstation
	default:
		return profile.TypeSSH
	}
}

// fromProtoCapability is a capability from the API.
func fromProtoCapability(c rookeryv1.Capability) Capability {
	switch c {
	case rookeryv1.Capability_CAPABILITY_EXEC:
		return CapabilityExec
	case rookeryv1.Capability_CAPABILITY_TERMINAL:
		return CapabilityTerminal
	case rookeryv1.Capability_CAPABILITY_FILES:
		return CapabilityFiles
	case rookeryv1.Capability_CAPABILITY_PORTS:
		return CapabilityPorts
	case rookeryv1.Capability_CAPABILITY_DESKTOP:
		return CapabilityDesktop
	case rookeryv1.Capability_CAPABILITY_CHANNEL:
		return CapabilityChannel
	case rookeryv1.Capability_CAPABILITY_ACTIONS:
		return CapabilityActions
	case rookeryv1.Capability_CAPABILITY_APPLY:
		return CapabilityApply
	default:
		return ""
	}
}

// toProtoProfile is a host profile as the API has it.
func toProtoProfile(p profile.Profile) *rookeryv1.HostProfile {
	converted := &rookeryv1.HostProfile{
		Type:       toProtoType(p.Type),
		Address:    p.Address,
		Port:       safeInt32(p.Port),
		SshUser:    p.SSHUser,
		Operator:   p.Operator,
		Tailnet:    p.Tailnet,
		Registries: p.Registries,
		VarsCbor:   p.Vars,
	}
	if p.Desktop != nil {
		converted.Desktop = &rookeryv1.Desktop{
			User:   p.Desktop.User,
			Width:  safeInt32(p.Desktop.Width),
			Height: safeInt32(p.Desktop.Height),
			Gpu:    p.Desktop.GPU,
		}
	}
	for _, feature := range p.Features {
		converted.Features = append(converted.Features, &rookeryv1.Feature{
			Plugin:   feature.Plugin,
			Feature:  feature.Feature,
			VarsCbor: feature.Vars,
		})
	}
	return converted
}

// fromProtoProfile is a host profile from the API.
func fromProtoProfile(p *rookeryv1.HostProfile) (profile.Profile, error) {
	if p == nil {
		return profile.Profile{}, fmt.Errorf("%w: a host needs a profile", access.ErrInvalid)
	}
	converted := profile.Profile{
		Type:       fromProtoType(p.GetType()),
		Address:    p.GetAddress(),
		Port:       int(p.GetPort()),
		SSHUser:    p.GetSshUser(),
		Operator:   p.GetOperator(),
		Tailnet:    p.GetTailnet(),
		Registries: p.GetRegistries(),
		Vars:       p.GetVarsCbor(),
	}
	if p.GetDesktop() != nil {
		if converted.Type != profile.TypeWorkstation {
			return profile.Profile{}, fmt.Errorf("%w: a %s host has no desktop", access.ErrInvalid, converted.Type)
		}
		converted.Desktop = &profile.Desktop{
			User:   p.GetDesktop().GetUser(),
			Width:  int(p.GetDesktop().GetWidth()),
			Height: int(p.GetDesktop().GetHeight()),
			GPU:    p.GetDesktop().GetGpu(),
		}
	}
	for _, feature := range p.GetFeatures() {
		converted.Features = append(converted.Features, profile.Feature{
			Plugin:  feature.GetPlugin(),
			Feature: feature.GetFeature(),
			Vars:    feature.GetVarsCbor(),
		})
	}
	return converted, nil
}

// credentialOf is a first contact credential from the API.
func credentialOf(c *rookeryv1.Credential) link.Credential {
	switch kind := c.GetKind().(type) {
	case *rookeryv1.Credential_Password:
		return link.Credential{Password: kind.Password}
	case *rookeryv1.Credential_PrivateKey:
		return link.Credential{PrivateKey: kind.PrivateKey}
	default:
		return link.Credential{}
	}
}

// safeInt32 is an int as an int32, clamped: a profile the API takes is one
// validate accepted, so a value that does not fit never reaches here.
func safeInt32(value int) int32 {
	if value > 1<<31-1 {
		return 1<<31 - 1
	}
	if value < -1<<31 {
		return -1 << 31
	}
	return int32(value)
}

// encode marshals a job's or an event's CBOR.
func encode(value any) ([]byte, error) {
	encoded, err := cborx.Marshal(value)
	if err != nil {
		return nil, fmt.Errorf("hosts: encoding: %w", err)
	}
	return encoded, nil
}

// decode unmarshals a job's or an event's CBOR.
func decode(data []byte, value any) error {
	if err := cborx.Unmarshal(data, value); err != nil {
		return fmt.Errorf("%w: reading the arguments: %w", access.ErrInvalid, err)
	}
	return nil
}
