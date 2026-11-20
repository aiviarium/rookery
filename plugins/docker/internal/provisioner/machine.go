package provisioner

import (
	"archive/tar"
	"bytes"
	"context"
	"fmt"
	"io"
	"strings"
	"time"

	cerrdefs "github.com/containerd/errdefs"
	"github.com/moby/moby/api/types/container"
	"github.com/moby/moby/api/types/mount"
	"github.com/moby/moby/api/types/network"
	"github.com/moby/moby/client"

	"github.com/aiviarium/rookery/pkg/sdk"
)

// seedPath is where cloud-init's NoCloud datasource reads a machine's seed: the
// provisioner copies user-data and meta-data there before the machine starts.
const seedPath = "/var/lib/cloud/seed/nocloud"

// diskPath is the machine's own storage, kept in a named volume so that a
// removed container leaves it.
const diskPath = "/var/lib/rookery"

// machineState is what the provisioner keeps to reach a machine again.
type machineState struct {
	Entry     string `cbor:"entry"`
	Container string `cbor:"container"`
	Network   string `cbor:"network"`
	Address   string `cbor:"address"`
}

// createMachine makes a machine: a container from Rookery's machine image,
// systemd as PID 1, its seed copied in, on the plugin's network, with a named
// volume for its disk.
func createMachine(ctx context.Context, opened *client.Client, arguments arguments, request sdk.CreateRequest) (sdk.Instance, error) {
	if request.Name == "" {
		return sdk.Instance{}, fmt.Errorf("a machine needs a name")
	}
	if !validName(request.Name) {
		return sdk.Instance{}, fmt.Errorf("%q is not a name a container can have", request.Name)
	}
	if strings.TrimSpace(request.Seed) == "" {
		return sdk.Instance{}, fmt.Errorf("a machine needs its cloud-init seed")
	}
	size, err := arguments.size()
	if err != nil {
		return sdk.Instance{}, err
	}
	runtime, err := chooseRuntime(ctx, opened, arguments)
	if err != nil {
		return sdk.Instance{}, err
	}
	if err := ensureImage(ctx, opened, arguments.Image); err != nil {
		return sdk.Instance{}, err
	}
	if err := ensureNetwork(ctx, opened, arguments.Network); err != nil {
		return sdk.Instance{}, err
	}

	labels := managedLabels(request.Entry, KindMachine, request.Name)
	containerName := dockerName("rookery", request.Name)
	diskName := dockerName("rookery", request.Entry, request.Name, "disk")
	seedName := dockerName("rookery", request.Entry, request.Name, "seed")
	for _, volume := range []string{diskName, seedName} {
		if _, err := opened.VolumeCreate(ctx, client.VolumeCreateOptions{Name: volume, Labels: labels}); err != nil {
			return sdk.Instance{}, fmt.Errorf("making the volume %s: %w", volume, err)
		}
	}

	config := &container.Config{
		Image:    arguments.Image,
		Hostname: request.Name,
		Labels:   labels,
	}
	host := machineHostConfig(size, runtime, arguments.Network, []mount.Mount{
		{Type: mount.TypeVolume, Source: diskName, Target: diskPath},
		{Type: mount.TypeVolume, Source: seedName, Target: seedPath},
	})
	networking := &network.NetworkingConfig{
		EndpointsConfig: map[string]*network.EndpointSettings{
			arguments.Network: {Aliases: []string{request.Name}},
		},
	}

	created, err := opened.ContainerCreate(ctx, client.ContainerCreateOptions{
		Config:           config,
		HostConfig:       host,
		NetworkingConfig: networking,
		Name:             containerName,
	})
	if err != nil {
		return sdk.Instance{}, fmt.Errorf("creating the machine's container: %w", err)
	}

	// The seed goes in before systemd starts, so cloud-init finds it on its first
	// boot. It is a tar of the two files, with no secret on a command line.
	seed, err := seedArchive(request.Name, request.Seed)
	if err != nil {
		discardContainer(ctx, opened, created.ID)
		return sdk.Instance{}, err
	}
	if _, err := opened.CopyToContainer(ctx, created.ID, client.CopyToContainerOptions{
		DestinationPath: seedPath,
		Content:         seed,
	}); err != nil {
		discardContainer(ctx, opened, created.ID)
		return sdk.Instance{}, fmt.Errorf("copying the seed into the machine: %w", err)
	}

	if _, err := opened.ContainerStart(ctx, created.ID, client.ContainerStartOptions{}); err != nil {
		discardContainer(ctx, opened, created.ID)
		return sdk.Instance{}, fmt.Errorf("starting the machine: %w", err)
	}

	address, err := addressOf(ctx, opened, created.ID, arguments.Network)
	if err != nil {
		return sdk.Instance{}, err
	}
	state, err := sdk.Marshal(machineState{
		Entry:     request.Entry,
		Container: created.ID,
		Network:   arguments.Network,
		Address:   address,
	})
	if err != nil {
		return sdk.Instance{}, err
	}
	return sdk.Instance{Kind: KindMachine, Name: request.Name, ID: created.ID, State: state, Address: address}, nil
}

// machineHostConfig is how a machine's container runs.
//
// It is never privileged: a privileged systemd reaches the host's udev, its lid
// switch and its power state, and has suspended the machine that ran it. sysbox
// gives systemd the namespaces it wants without that; without sysbox the
// container keeps its own cgroup namespace, mounts /run itself, and has only the
// capabilities systemd needs.
func machineHostConfig(chosen size, runtime machineRuntime, networkName string, mounts []mount.Mount) *container.HostConfig {
	caps := []string{}
	if runtime.name != sysbox {
		caps = machineCapabilities()
	}
	return &container.HostConfig{
		Runtime:      runtime.name,
		Privileged:   false,
		CgroupnsMode: container.CgroupnsModePrivate,
		CapAdd:       caps,
		NetworkMode:  container.NetworkMode(networkName),
		RestartPolicy: container.RestartPolicy{
			Name: container.RestartPolicyMode("unless-stopped"),
		},
		Resources: container.Resources{
			NanoCPUs: chosen.nanoCPUs(),
			Memory:   chosen.memory(),
		},
		// systemd wants /run and /run/lock to itself, and a container's /dev/shm
		// is too small for a desktop session.
		Tmpfs: map[string]string{
			"/run":      "rw,nosuid,nodev,size=65536k",
			"/run/lock": "rw,nosuid,nodev,noexec,relatime,size=5120k",
		},
		ShmSize: 64 << 20,
		Mounts:  mounts,
	}
}

// getMachine reports a machine's state and address.
func getMachine(ctx context.Context, opened *client.Client, instance sdk.Instance) (sdk.Instance, error) {
	state, err := machineStateOf(instance)
	if err != nil {
		return sdk.Instance{}, err
	}
	inspected, err := opened.ContainerInspect(ctx, state.Container, client.ContainerInspectOptions{})
	if err != nil {
		return sdk.Instance{}, fmt.Errorf("reading the machine %s: %w", instance.Name, err)
	}
	if settings, ok := inspected.Container.NetworkSettings.Networks[state.Network]; ok && settings.IPAddress.IsValid() {
		state.Address = settings.IPAddress.String()
	}
	instance.Address = state.Address
	instance.State, err = sdk.Marshal(state)
	if err != nil {
		return sdk.Instance{}, err
	}
	return instance, nil
}

// startMachine starts a stopped machine.
func startMachine(ctx context.Context, opened *client.Client, instance sdk.Instance) (sdk.Instance, error) {
	state, err := machineStateOf(instance)
	if err != nil {
		return sdk.Instance{}, err
	}
	if _, err := opened.ContainerStart(ctx, state.Container, client.ContainerStartOptions{}); err != nil {
		return sdk.Instance{}, fmt.Errorf("starting the machine %s: %w", instance.Name, err)
	}
	return getMachine(ctx, opened, instance)
}

// stopMachine stops a machine and keeps it.
func stopMachine(ctx context.Context, opened *client.Client, instance sdk.Instance) error {
	state, err := machineStateOf(instance)
	if err != nil {
		return err
	}
	timeout := 120
	if _, err := opened.ContainerStop(ctx, state.Container, client.ContainerStopOptions{Timeout: &timeout}); err != nil && !cerrdefs.IsNotFound(err) {
		return fmt.Errorf("stopping the machine %s: %w", instance.Name, err)
	}
	return nil
}

// removeMachine removes a machine's container and its volumes.
func removeMachine(ctx context.Context, opened *client.Client, instance sdk.Instance) error {
	state, err := machineStateOf(instance)
	if err != nil {
		return err
	}
	if err := removeContainer(ctx, opened, state.Container); err != nil {
		return err
	}
	for _, volume := range []string{
		dockerName("rookery", state.Entry, instance.Name, "disk"),
		dockerName("rookery", state.Entry, instance.Name, "seed"),
	} {
		if _, err := opened.VolumeRemove(ctx, volume, client.VolumeRemoveOptions{Force: true}); err != nil && !cerrdefs.IsNotFound(err) {
			return fmt.Errorf("removing the volume %s: %w", volume, err)
		}
	}
	return nil
}

// listMachines is the machines one entry made, matched by the labels the
// provisioner set.
func listMachines(ctx context.Context, opened *client.Client, request sdk.ListRequest) ([]sdk.Resource, error) {
	return listByKind(ctx, opened, KindMachine, request.Entry)
}

// listByKind is the containers of one kind, matched by the labels the
// provisioner set.
func listByKind(ctx context.Context, opened *client.Client, kind, entry string) ([]sdk.Resource, error) {
	filters := client.Filters{}.
		Add("label", LabelManaged+"=true").
		Add("label", LabelKind+"="+kind).
		Add("label", LabelEntry+"="+entry)
	found, err := opened.ContainerList(ctx, client.ContainerListOptions{All: true, Filters: filters})
	if err != nil {
		return nil, fmt.Errorf("listing the engine's containers: %w", err)
	}

	resources := make([]sdk.Resource, 0, len(found.Items))
	for _, summary := range found.Items {
		name := summary.Labels[LabelName]
		entry := summary.Labels[LabelEntry]
		networkName := summary.HostConfig.NetworkMode
		address := ""
		if summary.NetworkSettings != nil {
			address = networksAddress(summary.NetworkSettings.Networks, networkName)
		}
		if address == "" {
			// Not every engine reports an address in the list: ask it once more
			// for the container itself rather than list a machine without one.
			address, err = containerAddress(ctx, opened, summary.ID, networkName)
			if err != nil {
				return nil, err
			}
		}
		state, err := sdk.Marshal(machineState{Entry: entry, Container: summary.ID, Network: networkName, Address: address})
		if err != nil {
			return nil, err
		}
		labels := map[string]string{}
		for key, value := range summary.Labels {
			if strings.HasPrefix(key, strings.TrimSuffix(LabelPrefix, ".")) || strings.HasPrefix(key, "com.docker.compose.") {
				labels[key] = value
			}
		}
		resources = append(resources, sdk.Resource{
			Kind:    kind,
			Name:    name,
			ID:      summary.ID,
			Address: address,
			State:   state,
			Labels:  labels,
		})
	}
	return resources, nil
}

// machineStateOf reads what the provisioner stored about a machine.
func machineStateOf(instance sdk.Instance) (machineState, error) {
	var state machineState
	if len(instance.State) == 0 {
		// A resource the core recorded before this provisioner wrote state: the
		// container is named after the machine.
		return machineState{Container: dockerName("rookery", instance.Name)}, nil
	}
	if err := sdk.Unmarshal(instance.State, &state); err != nil {
		return machineState{}, fmt.Errorf("the state is not a machine's: %w", err)
	}
	return state, nil
}

// ensureImage pulls an image the engine does not have.
func ensureImage(ctx context.Context, opened *client.Client, image string) error {
	if _, err := opened.ImageInspect(ctx, image); err == nil {
		return nil
	} else if !cerrdefs.IsNotFound(err) {
		return fmt.Errorf("reading the image %s: %w", image, err)
	}
	pulled, err := opened.ImagePull(ctx, image, client.ImagePullOptions{})
	if err != nil {
		return fmt.Errorf("pulling the image %s: %w", image, err)
	}
	defer func() { _ = pulled.Close() }()
	if err := pulled.Wait(ctx); err != nil {
		return fmt.Errorf("pulling the image %s: %w", image, err)
	}
	return nil
}

// ensureNetwork makes the plugin's bridge network, which machines and
// deployments join.
func ensureNetwork(ctx context.Context, opened *client.Client, name string) error {
	if _, err := opened.NetworkInspect(ctx, name, client.NetworkInspectOptions{}); err == nil {
		return nil
	} else if !cerrdefs.IsNotFound(err) {
		return fmt.Errorf("reading the network %s: %w", name, err)
	}
	labels := map[string]string{LabelManaged: "true"}
	if _, err := opened.NetworkCreate(ctx, name, client.NetworkCreateOptions{Driver: "bridge", Labels: labels}); err != nil {
		return fmt.Errorf("making the network %s: %w", name, err)
	}
	return nil
}

// addressOf is a container's address on a network, once it is up.
func addressOf(ctx context.Context, opened *client.Client, containerID, networkName string) (string, error) {
	for attempt := 0; ; attempt++ {
		address, err := containerAddress(ctx, opened, containerID, networkName)
		if err != nil {
			return "", err
		}
		if address != "" {
			return address, nil
		}
		if attempt >= 50 {
			return "", fmt.Errorf("the machine has no address on %s", networkName)
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-time.After(100 * time.Millisecond):
		}
	}
}

// containerAddress is the address one container has on a network, empty when it
// has none yet.
func containerAddress(ctx context.Context, opened *client.Client, containerID, networkName string) (string, error) {
	inspected, err := opened.ContainerInspect(ctx, containerID, client.ContainerInspectOptions{})
	if err != nil {
		return "", fmt.Errorf("reading the machine's address: %w", err)
	}
	return networksAddress(inspected.Container.NetworkSettings.Networks, networkName), nil
}

// networksAddress is a container's address on the named network, or on the one
// it has, which is what an engine that names it differently reports.
func networksAddress(networks map[string]*network.EndpointSettings, networkName string) string {
	if settings, ok := networks[networkName]; ok && settings.IPAddress.IsValid() {
		return settings.IPAddress.String()
	}
	for _, settings := range networks {
		if settings.IPAddress.IsValid() {
			return settings.IPAddress.String()
		}
	}
	return ""
}

// discardContainer removes a container a failed create left half made, and says
// nothing when that fails too: the create's error is the one worth reporting.
func discardContainer(ctx context.Context, opened *client.Client, containerID string) {
	_ = removeContainer(ctx, opened, containerID)
}

// removeContainer removes a container and whatever it holds, ignoring one that is
// not there.
func removeContainer(ctx context.Context, opened *client.Client, containerID string) error {
	if containerID == "" {
		return nil
	}
	if _, err := opened.ContainerRemove(ctx, containerID, client.ContainerRemoveOptions{Force: true}); err != nil && !cerrdefs.IsNotFound(err) {
		return fmt.Errorf("removing the container: %w", err)
	}
	return nil
}

// seedArchive is a machine's seed as a tar: cloud-init's user-data and the
// meta-data that names the machine.
func seedArchive(host, userData string) (io.Reader, error) {
	var buffer bytes.Buffer
	archive := tar.NewWriter(&buffer)
	files := []struct {
		name    string
		mode    int64
		content string
	}{
		{"user-data", 0o600, userData},
		{"meta-data", 0o644, fmt.Sprintf("instance-id: %s\nlocal-hostname: %s\n", host, host)},
	}
	for _, file := range files {
		header := &tar.Header{
			Name:    file.name,
			Mode:    file.mode,
			Size:    int64(len(file.content)),
			ModTime: time.Unix(0, 0),
			Format:  tar.FormatPAX,
		}
		if err := archive.WriteHeader(header); err != nil {
			return nil, fmt.Errorf("writing the seed: %w", err)
		}
		if _, err := archive.Write([]byte(file.content)); err != nil {
			return nil, fmt.Errorf("writing the seed: %w", err)
		}
	}
	if err := archive.Close(); err != nil {
		return nil, fmt.Errorf("writing the seed: %w", err)
	}
	return &buffer, nil
}

// managedLabels are what the provisioner puts on everything it makes, which is
// how the core matches a resource to its record and how List finds it again.
func managedLabels(entry, kind, name string) map[string]string {
	return map[string]string{
		LabelManaged: "true",
		LabelEntry:   entry,
		LabelKind:    kind,
		LabelName:    name,
	}
}
