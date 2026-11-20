// Package provisioner is the Docker plugin's provisioner: machines for hosts,
// and compose projects, on a Docker engine.
package provisioner

import (
	"context"
	"errors"
	"fmt"
	"io"
	"strings"

	"github.com/moby/moby/client"

	"github.com/aiviarium/rookery/pkg/sdk"
)

// DefaultSocket is where a Docker engine listens on the machine that runs it.
const DefaultSocket = "/var/run/docker.sock"

// DefaultNetwork is the bridge network the machines and deployments join.
const DefaultNetwork = "rookery"

// DefaultMachineImage is the image a machine is made from, when an entry names
// none: images/machine.Dockerfile in Rookery's repository, published with the
// release.
const DefaultMachineImage = "ghcr.io/aiviarium/rookery/machine:24.04"

// Provisioner is the docker/container provisioner.
type Provisioner struct {
	// config is what the server gave this process: the socket DialNode is on,
	// and the token.
	config sdk.Config
}

// New returns the provisioner for a plugin process.
func New(config sdk.Config) *Provisioner { return &Provisioner{config: config} }

// Describe says what the Docker provisioner makes and takes.
func (p *Provisioner) Describe(context.Context) (sdk.Description, error) {
	return sdk.Description{
		Type:  "docker/container",
		Label: "Docker container",
		Kinds: []sdk.Kind{
			{ID: KindMachine, Label: "Machine", Machine: true},
			{ID: KindDeployment, Label: "Deployment"},
		},
		Arguments: []sdk.Argument{
			{Name: "image", Label: "Image", Type: "string", Help: "the image a machine runs; the default is Rookery's machine image"},
			{Name: "size", Label: "Size", Type: "select", Choices: []string{"small", "medium", "large"}, Help: "the machine's CPU and memory"},
			{Name: "runtime", Label: "Runtime", Type: "string", Help: "the container runtime; sysbox-runc is used when the engine has it"},
			{Name: "network", Label: "Network", Type: "string", Help: "the Docker network machines and deployments join"},
			{Name: "socket", Label: "Socket", Type: "string", Help: "the engine's socket on a node"},
			{Name: "file", Label: "Compose file", Type: "string", Help: "a deployment's compose file, on the server's machine"},
			{Name: "content", Label: "Compose content", Type: "string", Help: "a deployment's compose file, inline"},
			{Name: "project", Label: "Project", Type: "string", Help: "the compose project's name"},
			{Name: "environment", Label: "Environment", Type: "cbor", Help: "values the compose file's variables take"},
		},
		Sizes: []sdk.Size{
			{ID: "small", Label: "Small", CPUs: 2, MemoryMB: 2048, DiskGB: 20},
			{ID: "medium", Label: "Medium", CPUs: 4, MemoryMB: 8192, DiskGB: 50},
			{ID: "large", Label: "Large", CPUs: 8, MemoryMB: 16384, DiskGB: 100},
		},
		Images: []sdk.Image{
			{ID: DefaultMachineImage, Label: "Ubuntu 24.04", OS: "ubuntu", Release: "24.04", Architecture: "amd64"},
		},
		Actions: []sdk.Action{
			{Name: "images", Label: "Images", Group: "Docker"},
			{Name: "pull", Label: "Pull an image", Group: "Docker", Arguments: []sdk.Argument{
				{Name: "image", Label: "Image", Type: "string", Required: true},
			}},
			{Name: "volumes", Label: "Volumes", Group: "Docker"},
			{Name: "logs", Label: "Logs", Group: "Docker", Arguments: []sdk.Argument{
				{Name: "name", Label: "Deployment", Type: "string", Required: true},
				{Name: "tail", Label: "Lines", Type: "int"},
			}},
			{Name: "prune", Label: "Prune", Group: "Docker", Destructive: true},
		},
		Node:  true,
		Local: true,
	}, nil
}

// Status says whether the entry's engine answers.
func (p *Provisioner) Status(ctx context.Context, engine sdk.Engine, args []byte) (sdk.Status, error) {
	arguments, err := parseArguments(args)
	if err != nil {
		return sdk.Status{}, err
	}
	opened, err := p.open(ctx, engine, arguments)
	if err != nil {
		return sdk.Status{Message: err.Error()}, nil
	}
	defer func() { _ = opened.Close() }()

	version, err := opened.ServerVersion(ctx, client.ServerVersionOptions{})
	if err != nil {
		return sdk.Status{Message: err.Error()}, nil
	}
	return sdk.Status{Up: true, Message: "Docker " + version.Version}, nil
}

// Create makes a machine or a deployment.
func (p *Provisioner) Create(ctx context.Context, request sdk.CreateRequest) (sdk.Instance, error) {
	arguments, err := parseArguments(request.Args)
	if err != nil {
		return sdk.Instance{}, err
	}
	opened, err := p.open(ctx, request.Engine, arguments)
	if err != nil {
		return sdk.Instance{}, err
	}
	defer func() { _ = opened.Close() }()

	switch request.Kind {
	case KindMachine:
		return createMachine(ctx, opened, arguments, request)
	case KindDeployment:
		return createDeployment(ctx, opened, arguments, request)
	default:
		return sdk.Instance{}, fmt.Errorf("docker/container makes machines and deployments, not %s", request.Kind)
	}
}

// Get reports a machine's or a deployment's state.
func (p *Provisioner) Get(ctx context.Context, instance sdk.Instance) (sdk.Instance, error) {
	return p.withInstance(ctx, instance, func(ctx context.Context, opened *client.Client, instance sdk.Instance) (sdk.Instance, error) {
		switch instance.Kind {
		case KindMachine:
			return getMachine(ctx, opened, instance)
		case KindDeployment:
			return getDeployment(ctx, opened, instance)
		default:
			return sdk.Instance{}, fmt.Errorf("docker/container makes machines and deployments, not %s", instance.Kind)
		}
	})
}

// Start starts a machine or a deployment.
func (p *Provisioner) Start(ctx context.Context, instance sdk.Instance) (sdk.Instance, error) {
	return p.withInstance(ctx, instance, func(ctx context.Context, opened *client.Client, instance sdk.Instance) (sdk.Instance, error) {
		switch instance.Kind {
		case KindMachine:
			return startMachine(ctx, opened, instance)
		case KindDeployment:
			return startDeployment(ctx, opened, instance)
		default:
			return sdk.Instance{}, fmt.Errorf("docker/container makes machines and deployments, not %s", instance.Kind)
		}
	})
}

// Stop stops a machine or a deployment and keeps it.
func (p *Provisioner) Stop(ctx context.Context, instance sdk.Instance) error {
	_, err := p.withInstance(ctx, instance, func(ctx context.Context, opened *client.Client, instance sdk.Instance) (sdk.Instance, error) {
		switch instance.Kind {
		case KindMachine:
			return sdk.Instance{}, stopMachine(ctx, opened, instance)
		case KindDeployment:
			return sdk.Instance{}, stopDeployment(ctx, opened, instance)
		default:
			return sdk.Instance{}, fmt.Errorf("docker/container makes machines and deployments, not %s", instance.Kind)
		}
	})
	return err
}

// Remove removes a machine or a deployment and what it kept.
func (p *Provisioner) Remove(ctx context.Context, instance sdk.Instance) error {
	_, err := p.withInstance(ctx, instance, func(ctx context.Context, opened *client.Client, instance sdk.Instance) (sdk.Instance, error) {
		switch instance.Kind {
		case KindMachine:
			return sdk.Instance{}, removeMachine(ctx, opened, instance)
		case KindDeployment:
			return sdk.Instance{}, removeDeployment(ctx, opened, instance)
		default:
			return sdk.Instance{}, fmt.Errorf("docker/container makes machines and deployments, not %s", instance.Kind)
		}
	})
	return err
}

// List is what the entry made.
func (p *Provisioner) List(ctx context.Context, request sdk.ListRequest) ([]sdk.Resource, error) {
	arguments, err := parseArguments(request.Args)
	if err != nil {
		return nil, err
	}
	opened, err := p.open(ctx, request.Engine, arguments)
	if err != nil {
		return nil, err
	}
	defer func() { _ = opened.Close() }()

	switch request.Kind {
	case KindMachine:
		return listMachines(ctx, opened, request)
	case KindDeployment:
		return listDeployments(ctx, opened, request.Entry)
	case "":
		machines, err := listMachines(ctx, opened, request)
		if err != nil {
			return nil, err
		}
		deployments, err := listDeployments(ctx, opened, request.Entry)
		if err != nil {
			return nil, err
		}
		return append(machines, deployments...), nil
	default:
		return nil, nil
	}
}

// RunAction runs one of the provisioner's actions.
func (p *Provisioner) RunAction(ctx context.Context, request sdk.ActionRequest, line func(string)) error {
	arguments, err := parseArguments(request.EntryArgs)
	if err != nil {
		return err
	}
	opened, err := p.open(ctx, request.Engine, arguments)
	if err != nil {
		return err
	}
	defer func() { _ = opened.Close() }()

	switch request.Action {
	case "images":
		return imagesAction(ctx, opened, line)
	case "pull":
		return pullAction(ctx, opened, arguments, request.Args, line)
	case "volumes":
		return volumesAction(ctx, opened, arguments, line)
	case "logs":
		return logsAction(ctx, opened, arguments, request.Args, line)
	case "prune":
		return pruneAction(ctx, opened, line)
	default:
		return fmt.Errorf("docker/container has no action %q", request.Action)
	}
}

// withInstance opens the instance's engine and runs one operation with it.
func (p *Provisioner) withInstance(ctx context.Context, instance sdk.Instance, operation func(context.Context, *client.Client, sdk.Instance) (sdk.Instance, error)) (sdk.Instance, error) {
	opened, err := p.open(ctx, instance.Engine, arguments{})
	if err != nil {
		return sdk.Instance{}, err
	}
	defer func() { _ = opened.Close() }()
	return operation(ctx, opened, instance)
}

// open reaches the entry's engine.
func (p *Provisioner) open(ctx context.Context, engine sdk.Engine, arguments arguments) (*client.Client, error) {
	opened, err := openEngine(ctx, p.config, engine, arguments.socket())
	if err != nil {
		return nil, err
	}
	return opened, nil
}

// pullProgress turns an image pull into lines: the layers' progress is noisy, so
// only what finished and what failed is reported.
func pullProgress(progress io.Reader, line func(string)) error {
	if line == nil {
		_, err := io.Copy(io.Discard, progress)
		return err
	}
	buffer := make([]byte, 4096)
	for {
		n, err := progress.Read(buffer)
		if n > 0 {
			for _, text := range strings.Split(string(buffer[:n]), "\n") {
				text = strings.TrimSpace(text)
				if text != "" {
					line(text)
				}
			}
		}
		if err != nil {
			if errors.Is(err, io.EOF) {
				return nil
			}
			return err
		}
	}
}
