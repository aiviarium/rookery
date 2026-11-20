package server

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"

	"gopkg.in/yaml.v3"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/config"
	"github.com/aiviarium/rookery/internal/hosts"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/link"
	"github.com/aiviarium/rookery/internal/messaging"
	"github.com/aiviarium/rookery/internal/plugins"
	"github.com/aiviarium/rookery/internal/profile"
	"github.com/aiviarium/rookery/internal/provision"
	"github.com/aiviarium/rookery/internal/rollouts"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/tailnet"
)

type rolloutTargets struct {
	hosts    *hosts.Hosts
	version  string
	versions func(context.Context) (map[string]string, error)
}

func (t rolloutTargets) Hosts(ctx context.Context) ([]rollouts.Candidate, error) {
	versions, err := t.versions(ctx)
	if err != nil {
		return nil, err
	}
	records, err := t.hosts.List(ctx, hosts.Filter{})
	if err != nil {
		return nil, err
	}
	out := make([]rollouts.Candidate, 0, len(records))
	for _, record := range records {
		candidate := rollouts.Candidate{Name: record.Name, Labels: record.Labels}
		digest, err := profile.Digest(record.Profile, versions, t.version)
		if err != nil {
			candidate.Skip = err.Error()
		} else {
			candidate.Current = !record.Behind(digest)
		}
		if err := hosts.Check(record, hosts.CapabilityApply); err != nil {
			candidate.Skip = err.Error()
		}
		if err := hosts.Check(record, hosts.CapabilityActions); err != nil {
			candidate.ActionSkip = err.Error()
		}
		out = append(out, candidate)
	}
	return out, nil
}

func notifyJobFailure(notifications *messaging.Notifications, log *slog.Logger) func(context.Context, jobs.Job) {
	return func(ctx context.Context, job jobs.Job) {
		kind, user, _ := strings.Cut(job.CreatedBy, ":")
		if kind != access.KindUser || user == "" {
			return
		}
		if _, err := notifications.Send(ctx, messaging.Notification{
			UserID: user, Title: "Job failed", Body: "The " + job.Kind + " job failed.",
			Resource: access.JobURI(job.ID), Link: "/jobs/" + job.ID,
		}); err != nil {
			log.Error("sending job failure notification", slog.Any("error", err), slog.String("job", job.ID))
		}
	}
}

type provisionCatalogue struct{ installer *plugins.Installer }

func (c provisionCatalogue) ProvisionerTypes(ctx context.Context) ([]provision.Type, error) {
	installed, err := c.installer.List(ctx)
	if err != nil {
		return nil, err
	}
	var types provision.TypesOf
	platform := runtime.GOOS + "/" + runtime.GOARCH
	for _, plugin := range installed {
		if !plugin.Enabled {
			continue
		}
		var manifest plugins.Manifest
		if err := yaml.Unmarshal(plugin.Manifest, &manifest); err != nil {
			return nil, fmt.Errorf("provision: reading manifest for %s: %w", plugin.ID, err)
		}
		for _, entry := range manifest.Provisioners {
			command := ""
			if relative := entry.Command[platform]; relative != "" {
				command = filepath.Join(c.installer.Dir(plugin.ID, plugin.SHA256), relative)
			}
			types = append(types, provision.Type{
				ID: plugin.ID + "/" + entry.ID, Plugin: plugin.ID, Label: entry.Label, Kinds: entry.Kinds, Command: command,
			})
		}
	}
	return types.ProvisionerTypes(ctx)
}

func (c provisionCatalogue) ProvisionerType(ctx context.Context, id string) (provision.Type, bool, error) {
	types, err := c.ProvisionerTypes(ctx)
	if err != nil {
		return provision.Type{}, false, err
	}
	return provision.TypesOf(types).ProvisionerType(ctx, id)
}

type provisionCredentials struct {
	supervisor *plugins.Supervisor
	tokens     sync.Map
}

func (c *provisionCredentials) PluginToken(ctx context.Context, plugin string) (string, error) {
	if err := ctx.Err(); err != nil {
		return "", err
	}
	token := rand.Text()
	c.tokens.Store(token, plugin)
	context.AfterFunc(ctx, func() { c.tokens.Delete(token) })
	return token, nil
}

func (c *provisionCredentials) PluginByToken(token string) (string, bool) {
	if plugin, ok := c.tokens.Load(token); ok {
		return plugin.(string), true
	}
	return c.supervisor.PluginByToken(token)
}

func wireProvisioners(opened *store.Store, checker *access.Checker, secrets *config.Secrets, installer *plugins.Installer,
	supervisor *plugins.Supervisor, hostService *hosts.Service, keyRing *link.Ring, tail *tailnet.Manager,
	cfg Config, log *slog.Logger) (*provision.Service, *provisionCredentials, error) {
	directory := filepath.Join(cfg.StateDir, "calls")
	if err := os.MkdirAll(directory, 0o700); err != nil {
		return nil, nil, fmt.Errorf("creating provisioner call directory: %w", err)
	}
	credentials := &provisionCredentials{supervisor: supervisor}
	provisioners := provision.New(provision.Deps{
		Store: opened, Checker: checker, Catalog: provisionCatalogue{installer}, Secrets: secrets, Tokens: credentials,
		Runner: provision.NewRunner(cfg.Socket, directory, log), Logger: log,
		Hosts: machineHosts{service: hostService},
	})
	seeds := machineSeeds{ring: keyRing, tail: tail}
	return provision.NewService(provisioners, checker, seeds), credentials, nil
}

// machineHosts makes a provisioned machine a host: what the plan's life of a
// host records before the first contact and the apply.
type machineHosts struct {
	service *hosts.Service
}

func (m machineHosts) AddMachine(ctx context.Context, machine provision.MachineHost) error {
	hostType := profile.Type(machine.Type)
	if !hostType.Valid() {
		hostType = profile.TypeSSH
	}
	p := profile.Profile{Type: hostType, Address: machine.Address, Tailnet: true}
	if hostType == profile.TypeWorkstation {
		p.Desktop = &profile.Desktop{}
	}
	_, err := m.service.AddHost(ctx, hosts.AddInput{
		Name:      machine.Name,
		Type:      hostType,
		Profile:   p,
		CreatedBy: machine.CreatedBy,
	})
	return err
}

func (m machineHosts) RemoveHost(ctx context.Context, host string) error {
	err := m.service.RemoveHost(ctx, host)
	if errors.Is(err, access.ErrNotFound) {
		return nil
	}
	return err
}

// machineSeeds is what a created machine's cloud-init gets: the key ring's
// public key, and a one-use tailnet key when a tailnet is configured.
type machineSeeds struct {
	ring *link.Ring
	tail *tailnet.Manager
}

func (m machineSeeds) MachineSeed(ctx context.Context, host string) (provision.Seed, error) {
	key, err := m.ring.Active(ctx)
	if err != nil {
		return provision.Seed{}, fmt.Errorf("provision: reading the key ring: %w", err)
	}
	seed := provision.Seed{Host: host, PublicKey: key.Public}
	if m.tail != nil {
		tailKey, err := m.tail.MintHostKey(ctx, host)
		switch {
		case err == nil:
			seed.TailnetKey = tailKey.Secret
		case errors.Is(err, access.ErrUnavailable):
			// A deployment without a tailnet still makes a machine reachable
			// through the provisioner's own address.
		default:
			return provision.Seed{}, fmt.Errorf("provision: minting the tailnet key: %w", err)
		}
	}
	return seed, nil
}
