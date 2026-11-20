package hosts

import (
	"context"
	"fmt"
	"sync"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/link"
	"github.com/aiviarium/rookery/internal/profile"
)

// Runtime exposes the functions that use a running host, together with the
// terminal index rebuilt from each daemon.
type Runtime struct {
	hosts *Service
	mu    sync.RWMutex
	byID  map[string]terminalRecord
}

// NewRuntime creates the host feature services' shared state.
func NewRuntime(hosts *Service) *Runtime {
	return &Runtime{hosts: hosts, byID: make(map[string]terminalRecord)}
}

func (s *Service) authorizeHost(ctx context.Context, name string, level access.Level, capability Capability) (Record, error) {
	if err := validateName(name); err != nil {
		return Record{}, err
	}
	principal, _ := access.PrincipalFrom(ctx)
	if err := s.deps.Checker.Check(ctx, principal, access.HostURI(name), level); err != nil {
		return Record{}, err
	}
	record, err := s.deps.Hosts.Get(ctx, name)
	if err != nil {
		return Record{}, err
	}
	return record, Check(record, capability)
}

func (r *Runtime) use(ctx context.Context, name string, level access.Level, capability Capability) (*link.Client, func(), error) {
	if _, err := r.hosts.authorizeHost(ctx, name, level, capability); err != nil {
		return nil, nil, err
	}
	return r.hosts.deps.Links.Use(ctx, name)
}

func (r *Runtime) account(ctx context.Context, record Record, selector string) (*hostv1.User, bool, error) {
	if selector == "" {
		selector = "desktop"
	}
	name, err := hostAccount(record, selector)
	if err != nil {
		return nil, false, err
	}
	elevated := selector != "desktop"
	if elevated {
		principal, _ := access.PrincipalFrom(ctx)
		if err := r.hosts.deps.Checker.Check(ctx, principal, record.URI(), access.Manage); err != nil {
			return nil, false, err
		}
	}
	return &hostv1.User{Name: name}, elevated, nil
}

func hostAccount(record Record, selector string) (string, error) {
	switch selector {
	case "desktop":
		name := profile.DefaultDesktopUser
		if record.Profile.Desktop != nil {
			name = record.Profile.Desktop.User
		}
		if name == "" || name == "root" || name == record.Profile.Operator || name == record.Profile.SSHUser {
			return "", fmt.Errorf("%w: the desktop account must be separate from the SSH and operator accounts", access.ErrInvalid)
		}
		return name, nil
	case "operator":
		return record.Profile.Operator, nil
	case "root":
		return "root", nil
	default:
		return "", fmt.Errorf("%w: select desktop, operator or root", access.ErrInvalid)
	}
}

// ForgetTerminals drops the index for a host that has been removed or renamed.
func (r *Runtime) ForgetTerminals(name string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	for id, record := range r.byID {
		if record.host == name {
			delete(r.byID, id)
		}
	}
}
