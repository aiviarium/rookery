package provisioner

import (
	"context"
	"fmt"
	"net/netip"
	"strconv"
	"strings"

	"github.com/compose-spec/compose-go/v2/types"
	cerrdefs "github.com/containerd/errdefs"
	"github.com/moby/moby/api/types/container"
	"github.com/moby/moby/api/types/mount"
	"github.com/moby/moby/api/types/network"
	"github.com/moby/moby/client"

	"github.com/aiviarium/rookery/pkg/sdk"
)

// deploymentState is what the provisioner keeps to reach a deployment again: the
// compose project, whose containers, networks and volumes carry its labels.
type deploymentState struct {
	Entry   string `cbor:"entry"`
	Project string `cbor:"project"`
}

// createDeployment runs a compose project on the entry's engine: its networks and
// volumes, then its containers in dependency order.
func createDeployment(ctx context.Context, opened *client.Client, arguments arguments, request sdk.CreateRequest) (sdk.Instance, error) {
	if request.Name == "" {
		return sdk.Instance{}, fmt.Errorf("a deployment needs a name")
	}
	projectName := arguments.Project
	if projectName == "" {
		projectName = dockerName(request.Name)
	}
	project, err := loadProject(ctx, arguments, projectName)
	if err != nil {
		return sdk.Instance{}, err
	}
	labels := managedLabels(request.Entry, KindDeployment, request.Name)
	if err := applyProject(ctx, opened, project, labels); err != nil {
		return sdk.Instance{}, err
	}
	state, err := sdk.Marshal(deploymentState{Entry: request.Entry, Project: project.Name})
	if err != nil {
		return sdk.Instance{}, err
	}
	return sdk.Instance{Kind: KindDeployment, Name: request.Name, ID: project.Name, State: state}, nil
}

// applyProject makes the project's networks, volumes and containers, and starts
// them.
func applyProject(ctx context.Context, opened *client.Client, project *types.Project, labels map[string]string) error {
	networkNames := map[string]string{}
	for name, config := range project.Networks {
		full := projectNetworkName(project.Name, name, config)
		networkNames[name] = full
		if bool(config.External) {
			continue
		}
		merged := mergeLabels(labels, config.Labels)
		merged[ComposeProject] = project.Name
		if _, err := opened.NetworkInspect(ctx, full, client.NetworkInspectOptions{}); err == nil {
			continue
		} else if !cerrdefs.IsNotFound(err) {
			return fmt.Errorf("reading the network %s: %w", full, err)
		}
		driver := config.Driver
		if driver == "" {
			driver = "bridge"
		}
		if _, err := opened.NetworkCreate(ctx, full, client.NetworkCreateOptions{
			Driver: driver,
			Labels: merged,
		}); err != nil {
			return fmt.Errorf("making the network %s: %w", full, err)
		}
	}

	volumeNames := map[string]string{}
	for name, config := range project.Volumes {
		full := projectVolumeName(project.Name, name, config)
		volumeNames[name] = full
		if bool(config.External) {
			continue
		}
		merged := mergeLabels(labels, config.Labels)
		merged[ComposeProject] = project.Name
		if _, err := opened.VolumeCreate(ctx, client.VolumeCreateOptions{
			Name:       full,
			Driver:     config.Driver,
			DriverOpts: config.DriverOpts,
			Labels:     merged,
		}); err != nil {
			return fmt.Errorf("making the volume %s: %w", full, err)
		}
	}

	order, err := serviceOrder(project)
	if err != nil {
		return err
	}
	for _, name := range order {
		service := project.Services[name]
		if service.Build != nil && service.Image == "" {
			return fmt.Errorf("the service %s is built, which this provisioner does not do: give it an image", name)
		}
		if err := ensureImage(ctx, opened, service.Image); err != nil {
			return err
		}
		config, host, networking, containerName, err := containerForService(project, name, service, labels, networkNames, volumeNames)
		if err != nil {
			return err
		}
		created, err := opened.ContainerCreate(ctx, client.ContainerCreateOptions{
			Config:           config,
			HostConfig:       host,
			NetworkingConfig: networking,
			Name:             containerName,
		})
		if err != nil {
			return fmt.Errorf("creating the service %s: %w", name, err)
		}
		if _, err := opened.ContainerStart(ctx, created.ID, client.ContainerStartOptions{}); err != nil {
			return fmt.Errorf("starting the service %s: %w", name, err)
		}
	}
	return nil
}

// containerForService is one compose service as a container.
func containerForService(project *types.Project, name string, service types.ServiceConfig, labels map[string]string, networks map[string]string, volumes map[string]string) (*container.Config, *container.HostConfig, *network.NetworkingConfig, string, error) {
	merged := mergeLabels(labels, service.Labels)
	merged[ComposeProject] = project.Name
	merged[ComposeService] = name

	config := &container.Config{
		Image:      service.Image,
		Hostname:   service.Hostname,
		User:       service.User,
		WorkingDir: service.WorkingDir,
		Env:        environmentOf(service.Environment),
		Cmd:        service.Command,
		Entrypoint: service.Entrypoint,
		Labels:     merged,
	}
	if len(service.Ports) > 0 {
		config.ExposedPorts = network.PortSet{}
	}
	host := &container.HostConfig{
		RestartPolicy: container.RestartPolicy{Name: container.RestartPolicyMode(restartPolicy(service.Restart))},
		Privileged:    service.Privileged,
		CapAdd:        service.CapAdd,
		CapDrop:       service.CapDrop,
		Mounts:        mountsOf(service.Volumes, volumes),
		PortBindings:  network.PortMap{},
	}
	for _, published := range service.Ports {
		port, err := network.ParsePort(fmt.Sprintf("%d/%s", published.Target, protocolOf(published.Protocol)))
		if err != nil {
			return nil, nil, nil, "", fmt.Errorf("the service %s publishes %d: %w", name, published.Target, err)
		}
		if config.ExposedPorts == nil {
			config.ExposedPorts = network.PortSet{}
		}
		config.ExposedPorts[port] = struct{}{}
		hostPort, _ := strconv.Atoi(strings.Split(published.Published, "-")[0])
		address := netip.Addr{}
		if published.HostIP != "" {
			parsed, parseErr := netip.ParseAddr(published.HostIP)
			if parseErr != nil {
				return nil, nil, nil, "", fmt.Errorf("the service %s publishes from %q: %w", name, published.HostIP, parseErr)
			}
			address = parsed
		}
		host.PortBindings[port] = append(host.PortBindings[port], network.PortBinding{
			HostIP:   address,
			HostPort: strconv.Itoa(hostPort),
		})
	}

	// A service joins the networks it names, or the project's default one. The
	// endpoints are the engine's network names, which is what Docker knows them
	// by: a compose network called default is "<project>_default" there.
	endpoints := map[string]*network.EndpointSettings{}
	switch {
	case service.NetworkMode != "":
		host.NetworkMode = container.NetworkMode(service.NetworkMode)
	case len(service.Networks) > 0:
		for composeName := range service.Networks {
			engineName := networks[composeName]
			if engineName == "" {
				engineName = composeName
			}
			endpoints[engineName] = &network.EndpointSettings{Aliases: []string{name}}
		}
	default:
		if engineName := networks["default"]; engineName != "" {
			endpoints[engineName] = &network.EndpointSettings{Aliases: []string{name}}
		} else {
			for _, engineName := range networks {
				endpoints[engineName] = &network.EndpointSettings{Aliases: []string{name}}
			}
		}
	}
	if len(endpoints) > 0 {
		// The first network is the container's own: its address, and what
		// NetworkMode names when Docker asks.
		first := ""
		for engineName := range endpoints {
			if first == "" || engineName < first {
				first = engineName
			}
		}
		host.NetworkMode = container.NetworkMode(first)
	}
	networking := &network.NetworkingConfig{EndpointsConfig: endpoints}

	return config, host, networking, dockerName(project.Name, name, "1"), nil
}

// environmentOf is a compose environment as a container's.
func environmentOf(environment types.MappingWithEquals) []string {
	out := make([]string, 0, len(environment))
	for name, value := range environment {
		if value == nil {
			out = append(out, name)
			continue
		}
		out = append(out, name+"="+*value)
	}
	return out
}

// mountOf is one compose volume as a container mount.
func mountsOf(volumes []types.ServiceVolumeConfig, named map[string]string) []mount.Mount {
	out := make([]mount.Mount, 0, len(volumes))
	for _, volume := range volumes {
		entry := mount.Mount{Target: volume.Target, ReadOnly: volume.ReadOnly}
		switch volume.Type {
		case types.VolumeTypeVolume:
			entry.Type = mount.TypeVolume
			entry.Source = named[volume.Source]
			if entry.Source == "" {
				entry.Source = volume.Source
			}
		case types.VolumeTypeBind:
			entry.Type = mount.TypeBind
			entry.Source = volume.Source
		case types.VolumeTypeTmpfs:
			entry.Type = mount.TypeTmpfs
		default:
			continue
		}
		out = append(out, entry)
	}
	return out
}

// protocolOf is a published port's protocol, which defaults to tcp.
func protocolOf(protocol string) string {
	if protocol == "" {
		return "tcp"
	}
	return protocol
}

// restartPolicy is a compose restart as Docker's, which has no default.
func restartPolicy(restart string) string {
	switch restart {
	case "", "no":
		return "no"
	case "always", "unless-stopped":
		return restart
	default:
		return restart
	}
}

// mergeLabels is base with extra's entries on top.
func mergeLabels(base, extra map[string]string) map[string]string {
	out := make(map[string]string, len(base)+len(extra))
	for key, value := range base {
		out[key] = value
	}
	for key, value := range extra {
		out[key] = value
	}
	return out
}

// projectNetworkName is the engine's name for a compose network.
func projectNetworkName(project, name string, config types.NetworkConfig) string {
	if config.Name != "" {
		return config.Name
	}
	if name == "default" {
		return project + "_default"
	}
	return project + "_" + name
}

// projectVolumeName is the engine's name for a compose volume.
func projectVolumeName(project, name string, config types.VolumeConfig) string {
	if config.Name != "" {
		return config.Name
	}
	return project + "_" + name
}

// deploymentsOf is the deployments a project has, as the engine has them, grouped
// by project.
func deploymentsOf(ctx context.Context, opened *client.Client) (map[string][]container.Summary, error) {
	filters := client.Filters{}.
		Add("label", LabelManaged+"=true").
		Add("label", LabelKind+"="+KindDeployment)
	found, err := opened.ContainerList(ctx, client.ContainerListOptions{All: true, Filters: filters})
	if err != nil {
		return nil, fmt.Errorf("listing the engine's containers: %w", err)
	}
	projects := map[string][]container.Summary{}
	for _, summary := range found.Items {
		project := summary.Labels[ComposeProject]
		projects[project] = append(projects[project], summary)
	}
	return projects, nil
}

// withProject matches a project's containers by its label.
func withProject(ctx context.Context, opened *client.Client, project string) ([]container.Summary, error) {
	filters := client.Filters{}.Add("label", ComposeProject+"="+project)
	found, err := opened.ContainerList(ctx, client.ContainerListOptions{All: true, Filters: filters})
	if err != nil {
		return nil, fmt.Errorf("listing the project's containers: %w", err)
	}
	return found.Items, nil
}
