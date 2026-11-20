// Package provisioner is the Incus plugin's provisioner: virtual machines and
// system containers on an Incus server.
package provisioner

import (
	"context"
	"fmt"

	incus "github.com/lxc/incus/v6/client"

	"github.com/aiviarium/rookery/pkg/sdk"
)

// connect is how an engine is reached. Tests replace it with a fake.
type connect func(ctx context.Context, config sdk.Config, engine sdk.Engine, arguments arguments) (incus.InstanceServer, error)

// Provisioner is the incus/vm or incus/container provisioner. One process serves
// one of the two, because a provisioner describes one type.
type Provisioner struct {
	// config is what the server gave this process: the socket DialNode is on,
	// and the token.
	config sdk.Config

	// instanceType is TypeVM or TypeContainer.
	instanceType string

	// open reaches the entry's server.
	open connect
}

// New returns the provisioner for one of the plugin's two types.
func New(config sdk.Config, instanceType string) *Provisioner {
	return &Provisioner{config: config, instanceType: instanceType, open: openEngine}
}

// Describe says what the provisioner makes and takes.
func (p *Provisioner) Describe(context.Context) (sdk.Description, error) {
	label := "Incus container"
	help := "the cloud image a container runs"
	if p.instanceType == TypeVM {
		label = "Incus VM"
		help = "the cloud image a virtual machine runs"
	}
	return sdk.Description{
		Type:  p.instanceType,
		Label: label,
		Kinds: []sdk.Kind{{ID: KindMachine, Label: "Machine", Machine: true}},
		Arguments: []sdk.Argument{
			{Name: "image", Label: "Image", Type: "string", Help: help + "; the default is Ubuntu 24.04"},
			{Name: "size", Label: "Size", Type: "select", Choices: []string{"small", "medium", "large"}, Help: "the machine's CPU, memory and disk"},
			{Name: "profile", Label: "Profile", Type: "string", Help: "the Incus profile it takes; the default is default"},
			{Name: "project", Label: "Project", Type: "string", Help: "the Incus project it is made in"},
			{Name: "pool", Label: "Storage pool", Type: "string", Help: "the storage pool its disk comes from"},
			{Name: "socket", Label: "Socket", Type: "string", Help: "the Incus socket on a node"},
		},
		Secrets: []string{SecretCert, SecretKey},
		Sizes: []sdk.Size{
			{ID: "small", Label: "Small", CPUs: 2, MemoryMB: 2048, DiskGB: 20},
			{ID: "medium", Label: "Medium", CPUs: 4, MemoryMB: 8192, DiskGB: 50},
			{ID: "large", Label: "Large", CPUs: 8, MemoryMB: 16384, DiskGB: 100},
		},
		Images: []sdk.Image{
			{ID: "images:ubuntu/24.04", Label: "Ubuntu 24.04", OS: "ubuntu", Release: "24.04"},
			{ID: "images:debian/12", Label: "Debian 12", OS: "debian", Release: "12"},
		},
		Actions: []sdk.Action{
			{Name: "images", Label: "Images", Group: "Incus"},
			{Name: "instances", Label: "Instances", Group: "Incus"},
			{Name: "delete", Label: "Delete an instance", Group: "Incus", Destructive: true, Arguments: []sdk.Argument{
				{Name: "name", Label: "Instance", Type: "string", Required: true},
			}},
		},
		Node:  true,
		Local: true,
	}, nil
}

// Status says whether the entry's server answers.
func (p *Provisioner) Status(ctx context.Context, engine sdk.Engine, args []byte) (sdk.Status, error) {
	arguments, err := parseArguments(args)
	if err != nil {
		return sdk.Status{}, err
	}
	opened, err := p.open(ctx, p.config, engine, arguments)
	if err != nil {
		return sdk.Status{Message: err.Error()}, nil
	}
	server, _, err := opened.GetServer()
	if err != nil {
		return sdk.Status{Message: err.Error()}, nil
	}
	return sdk.Status{Up: true, Message: "Incus " + server.Environment.ServerVersion}, nil
}

// Create makes a machine.
func (p *Provisioner) Create(ctx context.Context, request sdk.CreateRequest) (sdk.Instance, error) {
	arguments, err := parseArguments(request.Args)
	if err != nil {
		return sdk.Instance{}, err
	}
	if request.Kind != KindMachine {
		return sdk.Instance{}, fmt.Errorf("%s makes machines, not %s", p.instanceType, request.Kind)
	}
	opened, err := p.open(ctx, p.config, request.Engine, arguments)
	if err != nil {
		return sdk.Instance{}, err
	}
	return createMachine(ctx, opened, p.instanceType, arguments, request)
}

// Get reports a machine's state and address.
func (p *Provisioner) Get(ctx context.Context, instance sdk.Instance) (sdk.Instance, error) {
	return p.withInstance(ctx, instance, func(ctx context.Context, opened incus.InstanceServer, instance sdk.Instance) (sdk.Instance, error) {
		return getMachine(ctx, opened, instance)
	})
}

// Start starts a stopped machine.
func (p *Provisioner) Start(ctx context.Context, instance sdk.Instance) (sdk.Instance, error) {
	return p.withInstance(ctx, instance, func(ctx context.Context, opened incus.InstanceServer, instance sdk.Instance) (sdk.Instance, error) {
		return startMachine(ctx, opened, instance)
	})
}

// Stop stops a machine and keeps it.
func (p *Provisioner) Stop(ctx context.Context, instance sdk.Instance) error {
	_, err := p.withInstance(ctx, instance, func(ctx context.Context, opened incus.InstanceServer, instance sdk.Instance) (sdk.Instance, error) {
		return sdk.Instance{}, stopMachine(ctx, opened, instance)
	})
	return err
}

// Remove removes a machine and its disk.
func (p *Provisioner) Remove(ctx context.Context, instance sdk.Instance) error {
	_, err := p.withInstance(ctx, instance, func(ctx context.Context, opened incus.InstanceServer, instance sdk.Instance) (sdk.Instance, error) {
		return sdk.Instance{}, removeMachine(ctx, opened, instance)
	})
	return err
}

// List is the machines one entry made.
func (p *Provisioner) List(ctx context.Context, request sdk.ListRequest) ([]sdk.Resource, error) {
	arguments, err := parseArguments(request.Args)
	if err != nil {
		return nil, err
	}
	if request.Kind != "" && request.Kind != KindMachine {
		return nil, nil
	}
	opened, err := p.open(ctx, p.config, request.Engine, arguments)
	if err != nil {
		return nil, err
	}
	return listMachines(ctx, opened, p.instanceType, request.Entry, arguments.Project)
}

// RunAction runs one of the provisioner's actions.
func (p *Provisioner) RunAction(ctx context.Context, request sdk.ActionRequest, line func(string)) error {
	arguments, err := parseArguments(request.EntryArgs)
	if err != nil {
		return err
	}
	opened, err := p.open(ctx, p.config, request.Engine, arguments)
	if err != nil {
		return err
	}
	switch request.Action {
	case "images":
		return imagesAction(ctx, opened, line)
	case "instances":
		return instancesAction(ctx, opened, line)
	case "delete":
		return deleteAction(ctx, opened, request.Args, line)
	default:
		return fmt.Errorf("%s has no action %q", p.instanceType, request.Action)
	}
}

// withInstance opens the instance's server and runs one operation with it.
func (p *Provisioner) withInstance(ctx context.Context, instance sdk.Instance, operation func(context.Context, incus.InstanceServer, sdk.Instance) (sdk.Instance, error)) (sdk.Instance, error) {
	opened, err := p.open(ctx, p.config, instance.Engine, arguments{})
	if err != nil {
		return sdk.Instance{}, err
	}
	return operation(ctx, opened, instance)
}
