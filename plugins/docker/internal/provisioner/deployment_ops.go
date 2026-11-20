package provisioner

import (
	"context"
	"fmt"
	"strconv"
	"strings"

	cerrdefs "github.com/containerd/errdefs"
	"github.com/moby/moby/api/types/container"
	"github.com/moby/moby/client"

	"github.com/aiviarium/rookery/pkg/sdk"
)

// getDeployment reports how much of a deployment is running.
func getDeployment(ctx context.Context, opened *client.Client, instance sdk.Instance) (sdk.Instance, error) {
	state, err := deploymentStateOf(instance)
	if err != nil {
		return sdk.Instance{}, err
	}
	containers, err := withProject(ctx, opened, state.Project)
	if err != nil {
		return sdk.Instance{}, err
	}
	if len(containers) == 0 {
		return sdk.Instance{}, fmt.Errorf("the deployment %s has no containers", instance.Name)
	}
	instance.State, err = sdk.Marshal(state)
	if err != nil {
		return sdk.Instance{}, err
	}
	instance.ID = state.Project
	return instance, nil
}

// startDeployment starts a deployment's containers.
func startDeployment(ctx context.Context, opened *client.Client, instance sdk.Instance) (sdk.Instance, error) {
	state, err := deploymentStateOf(instance)
	if err != nil {
		return sdk.Instance{}, err
	}
	containers, err := withProject(ctx, opened, state.Project)
	if err != nil {
		return sdk.Instance{}, err
	}
	for _, summary := range containers {
		if _, err := opened.ContainerStart(ctx, summary.ID, client.ContainerStartOptions{}); err != nil && !cerrdefs.IsNotFound(err) {
			return sdk.Instance{}, fmt.Errorf("starting the deployment %s: %w", instance.Name, err)
		}
	}
	return getDeployment(ctx, opened, instance)
}

// stopDeployment stops a deployment's containers.
func stopDeployment(ctx context.Context, opened *client.Client, instance sdk.Instance) error {
	state, err := deploymentStateOf(instance)
	if err != nil {
		return err
	}
	containers, err := withProject(ctx, opened, state.Project)
	if err != nil {
		return err
	}
	timeout := 30
	for _, summary := range containers {
		if _, err := opened.ContainerStop(ctx, summary.ID, client.ContainerStopOptions{Timeout: &timeout}); err != nil && !cerrdefs.IsNotFound(err) {
			return fmt.Errorf("stopping the deployment %s: %w", instance.Name, err)
		}
	}
	return nil
}

// removeDeployment removes a deployment's containers, networks and volumes.
func removeDeployment(ctx context.Context, opened *client.Client, instance sdk.Instance) error {
	state, err := deploymentStateOf(instance)
	if err != nil {
		return err
	}
	containers, err := withProject(ctx, opened, state.Project)
	if err != nil {
		return err
	}
	for _, summary := range containers {
		if _, err := opened.ContainerRemove(ctx, summary.ID, client.ContainerRemoveOptions{Force: true}); err != nil && !cerrdefs.IsNotFound(err) {
			return fmt.Errorf("removing the deployment %s: %w", instance.Name, err)
		}
	}

	networks, err := opened.NetworkList(ctx, client.NetworkListOptions{
		Filters: client.Filters{}.Add("label", ComposeProject+"="+state.Project),
	})
	if err != nil && !cerrdefs.IsNotFound(err) {
		return fmt.Errorf("listing the deployment's networks: %w", err)
	}
	for _, network := range networks.Items {
		if _, err := opened.NetworkRemove(ctx, network.ID, client.NetworkRemoveOptions{}); err != nil && !cerrdefs.IsNotFound(err) {
			return fmt.Errorf("removing the network %s: %w", network.Name, err)
		}
	}

	volumes, err := opened.VolumeList(ctx, client.VolumeListOptions{
		Filters: client.Filters{}.Add("label", ComposeProject+"="+state.Project),
	})
	if err != nil {
		return fmt.Errorf("listing the deployment's volumes: %w", err)
	}
	for _, volume := range volumes.Items {
		if _, err := opened.VolumeRemove(ctx, volume.Name, client.VolumeRemoveOptions{Force: true}); err != nil && !cerrdefs.IsNotFound(err) {
			return fmt.Errorf("removing the volume %s: %w", volume.Name, err)
		}
	}
	return nil
}

// listDeployments is every deployment any entry made, one per compose project.
func listDeployments(ctx context.Context, opened *client.Client, entry string) ([]sdk.Resource, error) {
	projects, err := deploymentsOf(ctx, opened)
	if err != nil {
		return nil, err
	}
	resources := make([]sdk.Resource, 0, len(projects))
	for project, containers := range projects {
		madeBy := ""
		name := project
		for _, summary := range containers {
			if madeBy == "" {
				madeBy = summary.Labels[LabelEntry]
			}
			if candidate := summary.Labels[LabelName]; candidate != "" {
				name = candidate
			}
		}
		if madeBy != entry {
			continue
		}
		state, err := sdk.Marshal(deploymentState{Entry: madeBy, Project: project})
		if err != nil {
			return nil, err
		}
		resources = append(resources, sdk.Resource{
			Kind:  KindDeployment,
			Name:  name,
			ID:    project,
			State: state,
			Labels: map[string]string{
				LabelManaged:   "true",
				LabelEntry:     madeBy,
				LabelKind:      KindDeployment,
				LabelName:      name,
				ComposeProject: project,
			},
		})
	}
	return resources, nil
}

// deploymentStateOf reads what the provisioner stored about a deployment.
func deploymentStateOf(instance sdk.Instance) (deploymentState, error) {
	var state deploymentState
	if err := sdk.Unmarshal(instance.State, &state); err != nil {
		return deploymentState{}, fmt.Errorf("the state is not a deployment's: %w", err)
	}
	if state.Project == "" {
		return deploymentState{}, fmt.Errorf("the state names no compose project")
	}
	return state, nil
}

// imagesAction lists the engine's images, one line each.
func imagesAction(ctx context.Context, opened *client.Client, line func(string)) error {
	found, err := opened.ImageList(ctx, client.ImageListOptions{})
	if err != nil {
		return fmt.Errorf("listing the engine's images: %w", err)
	}
	for _, image := range found.Items {
		tags := image.RepoTags
		if len(tags) == 0 {
			tags = []string{"<none>:<none>"}
		}
		for _, tag := range tags {
			line(fmt.Sprintf("%s\t%s\t%s", tag, friendlySize(image.Size), shortID(image.ID)))
		}
	}
	return nil
}

// pullAction pulls an image, reporting what the engine says.
func pullAction(ctx context.Context, opened *client.Client, _ arguments, args []byte, line func(string)) error {
	var request struct {
		Image string `cbor:"image"`
	}
	if len(args) > 0 {
		if err := sdk.Unmarshal(args, &request); err != nil {
			return fmt.Errorf("the arguments are not {image}: %w", err)
		}
	}
	if request.Image == "" {
		return fmt.Errorf("pull needs an image")
	}
	line("pulling " + request.Image)
	pulled, err := opened.ImagePull(ctx, request.Image, client.ImagePullOptions{})
	if err != nil {
		return fmt.Errorf("pulling %s: %w", request.Image, err)
	}
	defer func() { _ = pulled.Close() }()
	if err := pullProgress(pulled, line); err != nil {
		return fmt.Errorf("pulling %s: %w", request.Image, err)
	}
	line("pulled " + request.Image)
	return nil
}

// volumesAction lists the volumes the provisioner made.
func volumesAction(ctx context.Context, opened *client.Client, _ arguments, line func(string)) error {
	found, err := opened.VolumeList(ctx, client.VolumeListOptions{
		Filters: client.Filters{}.Add("label", LabelManaged+"=true"),
	})
	if err != nil {
		return fmt.Errorf("listing the engine's volumes: %w", err)
	}
	for _, volume := range found.Items {
		line(fmt.Sprintf("%s\t%s\t%s", volume.Name, volume.Driver, volume.Labels[LabelEntry]))
	}
	return nil
}

// logsAction is a deployment's containers' logs.
func logsAction(ctx context.Context, opened *client.Client, _ arguments, args []byte, line func(string)) error {
	var request struct {
		Name string `cbor:"name"`
		Tail int    `cbor:"tail"`
	}
	if len(args) > 0 {
		if err := sdk.Unmarshal(args, &request); err != nil {
			return fmt.Errorf("the arguments are not {name, tail}: %w", err)
		}
	}
	if request.Name == "" {
		return fmt.Errorf("logs needs the deployment's name")
	}
	tail := "100"
	if request.Tail > 0 {
		tail = strconv.Itoa(request.Tail)
	}

	filters := client.Filters{}.
		Add("label", LabelManaged+"=true").
		Add("label", LabelKind+"="+KindDeployment).
		Add("label", LabelName+"="+request.Name)
	found, err := opened.ContainerList(ctx, client.ContainerListOptions{All: true, Filters: filters})
	if err != nil {
		return fmt.Errorf("finding the deployment %s: %w", request.Name, err)
	}
	if len(found.Items) == 0 {
		return fmt.Errorf("no deployment is called %s", request.Name)
	}
	for _, summary := range found.Items {
		service := summary.Labels[ComposeService]
		logs, err := opened.ContainerLogs(ctx, summary.ID, client.ContainerLogsOptions{
			ShowStdout: true,
			ShowStderr: true,
			Tail:       tail,
		})
		if err != nil {
			return fmt.Errorf("reading the logs of %s: %w", service, err)
		}
		err = scanLines(logs, func(text string) { line(service + ": " + text) })
		_ = logs.Close()
		if err != nil {
			return fmt.Errorf("reading the logs of %s: %w", service, err)
		}
	}
	return nil
}

// pruneAction removes what Rookery made and nothing uses any more: stopped
// containers, and volumes no container holds.
func pruneAction(ctx context.Context, opened *client.Client, line func(string)) error {
	containers, err := opened.ContainerList(ctx, client.ContainerListOptions{
		All:     true,
		Filters: client.Filters{}.Add("label", LabelManaged+"=true"),
	})
	if err != nil {
		return fmt.Errorf("listing the engine's containers: %w", err)
	}
	for _, summary := range containers.Items {
		if summary.State == container.StateRunning {
			continue
		}
		if err := removeContainer(ctx, opened, summary.ID); err != nil {
			return err
		}
		line("removed the container " + shortID(summary.ID))
	}

	volumes, err := opened.VolumeList(ctx, client.VolumeListOptions{
		Filters: client.Filters{}.Add("label", LabelManaged+"=true"),
	})
	if err != nil {
		return fmt.Errorf("listing the engine's volumes: %w", err)
	}
	removed := 0
	for _, volume := range volumes.Items {
		if _, err := opened.VolumeRemove(ctx, volume.Name, client.VolumeRemoveOptions{}); err != nil {
			if cerrdefs.IsConflict(err) || cerrdefs.IsNotFound(err) {
				// A volume a container still holds is not this action's.
				continue
			}
			return fmt.Errorf("removing the volume %s: %w", volume.Name, err)
		}
		line("removed the volume " + volume.Name)
		removed++
	}
	line(fmt.Sprintf("removed %d volumes", removed))
	return nil
}

// scanLines calls sink with a stream's lines.
func scanLines(reader interface{ Read([]byte) (int, error) }, sink func(string)) error {
	buffer := make([]byte, 32*1024)
	rest := strings.Builder{}
	for {
		n, err := reader.Read(buffer)
		if n > 0 {
			rest.Write(buffer[:n])
			for {
				text := rest.String()
				index := strings.IndexByte(text, '\n')
				if index < 0 {
					break
				}
				sink(strings.TrimRight(text[:index], "\r"))
				rest.Reset()
				rest.WriteString(text[index+1:])
			}
		}
		if err != nil {
			if rest.Len() > 0 {
				sink(rest.String())
			}
			if err.Error() == "EOF" {
				return nil
			}
			return nil
		}
	}
}

// shortID is an id's first twelve characters, as Docker writes it.
func shortID(id string) string {
	if len(id) > 12 {
		return id[:12]
	}
	return id
}

// friendlySize is a byte count as people read it.
func friendlySize(bytes int64) string {
	const unit = 1024
	if bytes < unit {
		return strconv.FormatInt(bytes, 10) + " B"
	}
	divisor, exponent := int64(unit), 0
	for value := bytes / unit; value >= unit; value /= unit {
		divisor *= unit
		exponent++
	}
	return fmt.Sprintf("%.1f %cB", float64(bytes)/float64(divisor), "KMGTPE"[exponent])
}
