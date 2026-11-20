package provision

import (
	"context"
	"errors"
	"fmt"
	"log/slog"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/id"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/pkg/sdk"
)

// Secrets is where an entry's secrets come from: the values stored for
// provisioner/<name>, by the names the provisioner declared in its description.
// internal/config implements it.
type Secrets interface {
	ProvisionerSecrets(ctx context.Context, entry string, names []string) (map[string]string, error)
}

// Tokens mints the credential a provisioner's process presents on the server's
// socket. The plugin system implements it: a token bound to the plugin whose
// package carries the type.
type Tokens interface {
	PluginToken(ctx context.Context, plugin string) (string, error)
}

// Hosts is where a created machine becomes a host, and where a host's machine is
// looked up when the host goes. internal/hosts implements it.
type Hosts interface {
	// AddMachine records a host a provisioner made. It returns the profile the
	// host starts with, which the caller applies.
	AddMachine(ctx context.Context, machine MachineHost) error
}

// MachineHost is a machine a provisioner made, as its host record starts.
type MachineHost struct {
	// Name is the host's name.
	Name string

	// Address is where the machine answers, on the tailnet or the engine's
	// network.
	Address string

	// Provisioner is the entry that made it.
	Provisioner string

	// Kind is the resource kind it is, always machine.
	Kind string
}

// Deps is what Provisioners needs. Only Store and Catalog are required; a
// deployment without secrets, tokens or hosts still lists and calls.
type Deps struct {
	// Store is the database.
	Store *store.Store

	// Checker answers what a caller may do with an entry.
	Checker *access.Checker

	// Catalog is what provisioner types the installed plugins declare.
	Catalog Catalog

	// Runner starts a provisioner's executable for a call.
	Runner *Runner

	// Secrets are the entry's secrets, when the deployment stores them.
	Secrets Secrets

	// Tokens are the credentials a provisioner process presents on the server's
	// socket.
	Tokens Tokens

	// Hosts is where a created machine becomes a host.
	Hosts Hosts

	// Logger says what the provisioners did.
	Logger *slog.Logger
}

// Provisioners is the core's provisioners: their entries, what they made, and
// the calls into their executables.
type Provisioners struct {
	entries   *Entries
	resources *Resources
	catalog   Catalog
	runner    *Runner
	secrets   Secrets
	tokens    Tokens
	hosts     Hosts
	logger    *slog.Logger
}

// New returns the provisioners of a deployment.
func New(deps Deps) *Provisioners {
	logger := deps.Logger
	if logger == nil {
		logger = slog.New(slog.DiscardHandler)
	}
	return &Provisioners{
		entries:   NewEntries(deps.Store, deps.Checker),
		resources: NewResources(deps.Store),
		catalog:   deps.Catalog,
		runner:    deps.Runner,
		secrets:   deps.Secrets,
		tokens:    deps.Tokens,
		hosts:     deps.Hosts,
		logger:    logger,
	}
}

// Entries are the provisioner entries.
func (p *Provisioners) Entries() *Entries { return p.entries }

// Resources are what the entries made.
func (p *Provisioners) Resources() *Resources { return p.resources }

// Types is every provisioner type the installed plugins declare.
func (p *Provisioners) Types(ctx context.Context) ([]Type, error) {
	types, err := p.catalog.ProvisionerTypes(ctx)
	if err != nil {
		return nil, fmt.Errorf("provision: reading the provisioner types: %w", err)
	}
	return types, nil
}

// Type is one provisioner type by name, or ErrNoType.
func (p *Provisioners) Type(ctx context.Context, name string) (Type, error) {
	typ, found, err := p.catalog.ProvisionerType(ctx, name)
	if err != nil {
		return Type{}, fmt.Errorf("provision: reading the provisioner type %s: %w", name, err)
	}
	if !found {
		return Type{}, fmt.Errorf("%w: no installed plugin declares the provisioner type %s", ErrNoType, name)
	}
	return typ, nil
}

// Describe asks a type what it makes and takes. The type's executable is started
// for the call, with the credential of the plugin that carries it, and stopped
// after it.
func (p *Provisioners) Describe(ctx context.Context, typ Type) (sdk.Description, error) {
	var description sdk.Description
	err := p.call(ctx, Call{Type: typ}, func(callCtx context.Context, client *sdk.ProvisionerClient) error {
		described, err := client.Describe(callCtx)
		if err != nil {
			return err
		}
		description = described
		return nil
	})
	if err != nil {
		return sdk.Description{}, fmt.Errorf("provision: describing %s: %w", typ.ID, err)
	}
	return description, nil
}

// Status says whether an entry's engine answers.
func (p *Provisioners) Status(ctx context.Context, name string) (sdk.Status, error) {
	entry, typ, err := p.entryAndType(ctx, name)
	if err != nil {
		return sdk.Status{}, err
	}
	var status sdk.Status
	err = p.call(ctx, Call{Entry: entry, Type: typ}, func(callCtx context.Context, client *sdk.ProvisionerClient) error {
		answered, err := client.Status(callCtx, engineOf(entry), entry.Args)
		if err != nil {
			return err
		}
		status = answered
		return nil
	})
	if err != nil {
		return sdk.Status{}, fmt.Errorf("provision: asking %s for its status: %w", name, err)
	}
	return status, nil
}

// RunAction runs one of an entry's type's actions, sending each line of its
// output to line.
func (p *Provisioners) RunAction(ctx context.Context, name, action string, args []byte, line func(string)) error {
	entry, typ, err := p.entryAndType(ctx, name)
	if err != nil {
		return err
	}
	err = p.call(ctx, Call{Entry: entry, Type: typ, Line: line}, func(callCtx context.Context, client *sdk.ProvisionerClient) error {
		return client.RunAction(callCtx, sdk.ActionRequest{
			Engine:    engineOf(entry),
			Action:    action,
			Args:      args,
			EntryArgs: entry.Args,
		}, line)
	})
	if err != nil {
		return fmt.Errorf("provision: running %s on %s: %w", action, name, err)
	}
	return nil
}

// ListMade is what a provisioner says its entry made, matched by the labels it
// set. The core's own records are what an edge call reads; this is how a caller
// compares them with the engine.
func (p *Provisioners) ListMade(ctx context.Context, name, kind string) ([]sdk.Resource, error) {
	entry, typ, err := p.entryAndType(ctx, name)
	if err != nil {
		return nil, err
	}
	var resources []sdk.Resource
	err = p.call(ctx, Call{Entry: entry, Type: typ}, func(callCtx context.Context, client *sdk.ProvisionerClient) error {
		made, err := client.List(callCtx, sdk.ListRequest{
			Engine: engineOf(entry),
			Kind:   kind,
			Args:   entry.Args,
			Entry:  entry.Name,
		})
		if err != nil {
			return err
		}
		resources = made
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("provision: listing what %s made: %w", name, err)
	}
	return resources, nil
}

// CreateMachine makes a host's machine through an entry, records it, and hands it
// to the hosts when the deployment has them.
//
// The machine's cloud-init seed is rendered here: the operator account with the
// key ring's public key and passwordless sudo, and Tailscale with a one-use auth
// key. Everything else the host needs is the apply that follows.
func (p *Provisioners) CreateMachine(ctx context.Context, request MachineRequest) (Resource, error) {
	if !validHostName(request.Host) {
		return Resource{}, fmt.Errorf("%w: %q is not a host name", access.ErrInvalid, request.Host)
	}
	entry, typ, err := p.entryAndType(ctx, request.Entry)
	if err != nil {
		return Resource{}, err
	}
	if !typ.hasKind(KindMachine) {
		return Resource{}, fmt.Errorf("%w: %s makes no machine", access.ErrInvalid, typ.ID)
	}
	if _, taken, err := p.resources.MachineOf(ctx, request.Host); err != nil {
		return Resource{}, err
	} else if taken {
		return Resource{}, fmt.Errorf("%w: %s is a machine already", access.ErrConflict, request.Host)
	}

	seed := request.Seed
	seed.Host = request.Host
	contents, err := RenderSeed(seed)
	if err != nil {
		return Resource{}, err
	}

	machineArgs := request.Args
	if len(machineArgs) == 0 {
		machineArgs = entry.Args
	}

	var instance sdk.Instance
	err = p.callWithSecrets(ctx, entry, typ, request.Line, func(callCtx context.Context, client *sdk.ProvisionerClient) error {
		created, err := client.Create(callCtx, sdk.CreateRequest{
			Engine: engineOf(entry),
			Entry:  entry.Name,
			Kind:   KindMachine,
			Name:   request.Host,
			Args:   machineArgs,
			Seed:   contents,
		})
		if err != nil {
			return err
		}
		instance = created
		return nil
	})
	if err != nil {
		return Resource{}, fmt.Errorf("provision: creating the machine %s: %w", request.Host, err)
	}

	resource, err := p.resources.Create(ctx, Resource{
		ID:          id.New(),
		Kind:        KindMachine,
		Name:        request.Host,
		Provisioner: entry.Name,
		State:       instance.State,
		Address:     instance.Address,
	})
	if err != nil {
		// The machine exists and the record does not: remove it rather than leave
		// something the core cannot find.
		if removeErr := p.removeInstance(ctx, entry, typ, instance, request.Line); removeErr != nil {
			p.logger.Error("a machine the core could not record was left behind",
				slog.String("host", request.Host),
				slog.String("error", removeErr.Error()),
			)
		}
		return Resource{}, err
	}

	if p.hosts != nil {
		if err := p.hosts.AddMachine(ctx, MachineHost{
			Name:        request.Host,
			Address:     resource.Address,
			Provisioner: entry.Name,
			Kind:        resource.Kind,
		}); err != nil {
			return Resource{}, fmt.Errorf("provision: recording the host %s: %w", request.Host, err)
		}
	}
	return resource, nil
}

// RemoveMachine removes a host's machine and forgets it.
func (p *Provisioners) RemoveMachine(ctx context.Context, host string, line func(string)) error {
	machine, found, err := p.resources.MachineOf(ctx, host)
	if err != nil {
		return err
	}
	if !found {
		return fmt.Errorf("%w: %s has no machine", access.ErrNotFound, host)
	}
	entry, typ, err := p.entryAndType(ctx, machine.Provisioner)
	if err != nil {
		return err
	}
	if err := p.removeInstance(ctx, entry, typ, sdk.Instance{
		Engine: engineOf(entry),
		Kind:   machine.Kind,
		Name:   machine.Name,
		State:  machine.State,
	}, line); err != nil {
		return err
	}
	return p.resources.Delete(ctx, machine.ID)
}

// Offer adds the entries a host feature offers when it is applied to a node: one
// per type, named <node>-<type>, driven through that node's engine.
func (p *Provisioners) Offer(ctx context.Context, node string, offers []string) ([]Entry, error) {
	if node == "" {
		return nil, fmt.Errorf("%w: a feature that offers provisioners is applied to a node", access.ErrInvalid)
	}
	added := make([]Entry, 0, len(offers))
	for _, offer := range offers {
		typ, err := p.Type(ctx, offer)
		if err != nil {
			return nil, err
		}
		name := node + "-" + typ.ID
		existing, err := p.entries.ByName(ctx, name)
		if err == nil {
			if existing.Node != node {
				return nil, fmt.Errorf("%w: %s is there already, on %s", access.ErrConflict, name, existing.Node)
			}
			added = append(added, existing)
			continue
		}
		if !errors.Is(err, access.ErrNotFound) {
			return nil, err
		}
		entry, err := p.entries.Add(ctx, Entry{Name: name, Type: typ.ID, Plugin: typ.Plugin, Args: []byte{0xa0}, Node: node})
		if err != nil {
			return nil, err
		}
		added = append(added, entry)
	}
	return added, nil
}

// entryAndType reads an entry and the type it names.
func (p *Provisioners) entryAndType(ctx context.Context, name string) (Entry, Type, error) {
	entry, err := p.entries.ByName(ctx, name)
	if err != nil {
		return Entry{}, Type{}, err
	}
	typ, err := p.Type(ctx, entry.Type)
	if err != nil {
		return Entry{}, Type{}, err
	}
	return entry, typ, nil
}

// call starts a provisioner for one call without secrets.
func (p *Provisioners) call(ctx context.Context, call Call, fn func(context.Context, *sdk.ProvisionerClient) error) error {
	return p.callWithSecrets(ctx, call.Entry, call.Type, call.Line, fn)
}

// callWithSecrets starts a provisioner for one call, gives it the secrets its
// description declares, and stops it afterwards.
func (p *Provisioners) callWithSecrets(ctx context.Context, entry Entry, typ Type, line func(string), fn func(context.Context, *sdk.ProvisionerClient) error) error {
	if p.runner == nil {
		return fmt.Errorf("%w: no provisioner runner", access.ErrUnavailable)
	}
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	call := Call{Entry: entry, Type: typ, Engine: engineOf(entry), Line: line}
	if p.tokens != nil {
		token, err := p.tokens.PluginToken(ctx, typ.Plugin)
		if err != nil {
			return fmt.Errorf("provision: minting the provisioner's token: %w", err)
		}
		call.Token = token
	}
	var secrets map[string]string
	if p.secrets != nil && entry.Name != "" {
		names, err := p.declaredSecrets(ctx, typ)
		if err != nil {
			return err
		}
		if len(names) > 0 {
			values, err := p.secrets.ProvisionerSecrets(ctx, entry.Name, names)
			if err != nil {
				return fmt.Errorf("provision: reading the secrets of %s: %w", entry.Name, err)
			}
			secrets = values
		}
	}
	call.Secrets = secrets
	return p.runner.Do(ctx, call, fn)
}

// declaredSecrets is the secret names a type's description declares.
func (p *Provisioners) declaredSecrets(ctx context.Context, typ Type) ([]string, error) {
	description, err := p.Describe(ctx, typ)
	if err != nil {
		return nil, err
	}
	return description.Secrets, nil
}

// removeInstance removes what a provisioner made, without touching the records.
func (p *Provisioners) removeInstance(ctx context.Context, entry Entry, typ Type, instance sdk.Instance, line func(string)) error {
	return p.callWithSecrets(ctx, entry, typ, line, func(callCtx context.Context, client *sdk.ProvisionerClient) error {
		return client.Remove(callCtx, instance)
	})
}

// engineOf is the entry's engine, as a provisioner sees it.
func engineOf(entry Entry) sdk.Engine {
	return sdk.Engine{Node: entry.Node, Remote: entry.Remote}
}

// hasKind reports whether a type makes a kind.
func (t Type) hasKind(kind string) bool {
	for _, candidate := range t.Kinds {
		if candidate == kind {
			return true
		}
	}
	return false
}

// validHostName reports whether a name can be a host's: what a DNS label is,
// which is what the host's tailnet name becomes too.
func validHostName(name string) bool {
	if name == "" || len(name) > 63 || name[0] == '-' || name[len(name)-1] == '-' {
		return false
	}
	for _, c := range name {
		switch {
		case c >= 'a' && c <= 'z', c >= '0' && c <= '9', c == '-':
		default:
			return false
		}
	}
	return true
}

// MachineRequest is a host to create through an entry.
type MachineRequest struct {
	// Args are the machine's arguments. Empty uses the entry defaults.
	Args []byte
	// Host is the host's name.
	Host string

	// Entry is the provisioner entry to create it through.
	Entry string

	// Seed is what the machine's cloud-init needs, less the host's name.
	Seed Seed

	// Line is where the provisioner's output goes.
	Line func(string)
}
