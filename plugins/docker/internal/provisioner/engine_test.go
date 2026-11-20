package provisioner

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"testing"
	"time"

	"github.com/moby/moby/api/types/container"
	"github.com/moby/moby/api/types/mount"
	"github.com/moby/moby/client"

	"github.com/aiviarium/rookery/pkg/sdk"
)

// The tests in this file run against the machine's own Docker engine: the
// provisioner's local engine is what a server with Docker installed drives. They
// skip when there is no engine, and they are not parallel, because they make
// containers on one engine.
//
// They run plain containers: an alpine service in a deployment, and the actions
// that read an engine. A machine runs systemd as PID 1, which touches cgroups,
// mounts and udev in ways that belong on a runner rather than on somebody's
// laptop, so those tests skip unless ROOKERY_TEST_DOCKER_SYSTEMD=1. Even there,
// a machine is never privileged: sysbox or an unprivileged container with its own
// cgroup namespace.

// systemdTests runs a test of a machine, or skips it.
func systemdTests(t *testing.T) {
	t.Helper()

	engine(t)
	if os.Getenv("ROOKERY_TEST_DOCKER_SYSTEMD") != "1" {
		t.Skip("a machine runs systemd: set ROOKERY_TEST_DOCKER_SYSTEMD=1 to test one")
	}
}

// engine is the machine's Docker engine, or a skip.
func engine(t *testing.T) *client.Client {
	t.Helper()

	opened, err := client.New(client.WithHostFromEnv())
	if err != nil {
		t.Skipf("no Docker engine: %v", err)
	}
	t.Cleanup(func() { _ = opened.Close() })

	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	if _, err := opened.Ping(ctx, client.PingOptions{}); err != nil {
		t.Skipf("no Docker engine: %v", err)
	}
	return opened
}

// provisioner is the provisioner with the machine's engine.
func provisioner(t *testing.T) *Provisioner {
	t.Helper()

	engine(t)
	return New(sdk.Config{})
}

// machineImage is the image the machine tests use: the env's, or
// images/machine.Dockerfile built once for the test run.
func machineImage(t *testing.T) string {
	t.Helper()

	if image := os.Getenv("ROOKERY_TEST_MACHINE_IMAGE"); image != "" {
		return image
	}
	tag := "rookery/machine:test"
	//nolint:gosec // the test builds the image from the repository's Dockerfile.
	build := exec.CommandContext(t.Context(), "docker", "build", "-q", "-t", tag,
		"-f", "../../../../images/machine.Dockerfile", "../../../../images")
	if output, err := build.CombinedOutput(); err != nil {
		t.Fatalf("building the machine image: %v\n%s", err, output)
	}
	return tag
}

// hostName is a name no other test's container has.
func hostName(prefix string) string {
	return fmt.Sprintf("%s-%d", prefix, time.Now().UnixNano())
}

// localEngine is the engine an entry with no node and no remote has.
func localEngine() sdk.Engine { return sdk.Engine{} }

// TestDescribe checks what the provisioner says about itself.
func TestDescribe(t *testing.T) {
	t.Parallel()

	description, err := New(sdk.Config{}).Describe(t.Context())
	if err != nil {
		t.Fatalf("describing: %v", err)
	}
	if description.Type != "docker/container" {
		t.Fatalf("the type is %q", description.Type)
	}
	var kinds []string
	for _, kind := range description.Kinds {
		kinds = append(kinds, kind.ID)
	}
	if strings.Join(kinds, ",") != KindMachine+","+KindDeployment {
		t.Fatalf("the kinds are %v", kinds)
	}
	if !description.Kinds[0].Machine {
		t.Fatal("machine is not the core's kind")
	}
	if len(description.Actions) < 4 {
		t.Fatalf("the actions are %+v", description.Actions)
	}
	for _, argument := range description.Arguments {
		if strings.Contains(argument.Name, "privileged") {
			t.Fatalf("the description offers %q: a machine is never privileged", argument.Name)
		}
	}
	if !description.Node || !description.Local {
		t.Fatalf("the description does not say where it runs: %+v", description)
	}
}

// TestMachineHostConfig checks that a machine is never privileged, whether the
// engine has sysbox or not.
func TestMachineHostConfig(t *testing.T) {
	t.Parallel()

	chosen, err := (arguments{Size: "medium"}).size()
	if err != nil {
		t.Fatalf("reading the size: %v", err)
	}
	mounts := []mount.Mount{{Type: mount.TypeVolume, Source: "disk", Target: diskPath}}

	unprivileged := machineHostConfig(chosen, machineRuntime{}, "rookery", mounts)
	if unprivileged.Privileged {
		t.Fatal("the machine is privileged")
	}
	if unprivileged.CgroupnsMode != container.CgroupnsModePrivate {
		t.Fatalf("the cgroup namespace is %q", unprivileged.CgroupnsMode)
	}
	for _, needed := range machineCapabilities() {
		if !contains(unprivileged.CapAdd, needed) {
			t.Errorf("the unprivileged machine has no %s", needed)
		}
	}
	if unprivileged.NanoCPUs != int64(4*1e9) || unprivileged.Memory != 8192<<20 {
		t.Fatalf("the machine's resources are %+v", unprivileged.Resources)
	}
	if unprivileged.Tmpfs["/run"] == "" || unprivileged.Tmpfs["/run/lock"] == "" {
		t.Fatalf("the machine's /run is not its own: %v", unprivileged.Tmpfs)
	}
	if unprivileged.NetworkMode != "rookery" || len(unprivileged.Mounts) != 1 {
		t.Fatalf("the machine's host config is %+v", unprivileged)
	}

	// sysbox gives systemd everything itself: no capability is added on top.
	withSysbox := machineHostConfig(chosen, machineRuntime{name: sysbox}, "rookery", mounts)
	if withSysbox.Runtime != sysbox || len(withSysbox.CapAdd) != 0 {
		t.Fatalf("the sysbox machine is %+v", withSysbox)
	}
	if withSysbox.Privileged {
		t.Fatal("the sysbox machine is privileged")
	}
}

// TestMachineRefusals checks what a machine is refused before any container is
// made.
func TestMachineRefusals(t *testing.T) {
	t.Parallel()

	for name, request := range map[string]sdk.CreateRequest{
		"no name": {Entry: "lab-docker/container", Kind: KindMachine, Seed: "#cloud-config\n"},
		"a name a container cannot have": {
			Entry: "lab-docker/container", Kind: KindMachine, Name: "Desk/1", Seed: "#cloud-config\n",
		},
		"no seed": {Entry: "lab-docker/container", Kind: KindMachine, Name: "desk-1"},
	} {
		if _, err := createMachine(t.Context(), nil, arguments{Image: DefaultMachineImage}, request); err == nil {
			t.Errorf("%s worked", name)
		}
	}
	if _, err := (arguments{Size: "huge"}).size(); err == nil {
		t.Error("a size that is not one worked")
	}
}

// TestStatus checks that the machine's engine answers, and that an engine that
// is not there does not.
func TestStatus(t *testing.T) {
	provisioner := provisioner(t)

	status, err := provisioner.Status(t.Context(), localEngine(), nil)
	if err != nil {
		t.Fatalf("reading the status: %v", err)
	}
	if !status.Up || !strings.HasPrefix(status.Message, "Docker ") {
		t.Fatalf("the status is %+v", status)
	}

	unreachable, err := provisioner.Status(t.Context(), sdk.Engine{Node: "lab"}, nil)
	if err != nil {
		t.Fatalf("reading a node's status without a socket: %v", err)
	}
	if unreachable.Up {
		t.Fatalf("a node with no server answered up: %+v", unreachable)
	}
	if !strings.Contains(unreachable.Message, sdk.SocketEnv) {
		t.Fatalf("the message is %q", unreachable.Message)
	}
}

// TestMachine makes a machine, lists it, stops it, starts it and removes it.
func TestMachine(t *testing.T) {
	systemdTests(t)
	provisioner := provisioner(t)
	image := machineImage(t)
	name := hostName("machine")
	ctx := t.Context()

	args := mustMarshal(t, map[string]any{"image": image, "size": "small", "network": "rookery"})
	request := sdk.CreateRequest{Engine: localEngine(), Entry: "lab-docker/container", Kind: KindMachine, Name: name, Args: args, Seed: "#cloud-config\nhostname: " + name + "\n"}
	instance, err := provisioner.Create(ctx, request)
	if err != nil {
		t.Fatalf("creating the machine: %v", err)
	}
	t.Cleanup(func() {
		_ = provisioner.Remove(context.WithoutCancel(ctx), instance)
	})
	if instance.Address == "" {
		t.Fatal("the machine has no address")
	}
	if instance.Kind != KindMachine || instance.Name != name {
		t.Fatalf("the machine is %+v", instance)
	}

	// Its seed is in the volume cloud-init reads, before systemd started.
	opened := engine(t)
	seed, err := opened.ContainerStatPath(ctx, instance.ID, client.ContainerStatPathOptions{Path: seedPath + "/user-data"})
	if err != nil {
		t.Fatalf("the machine has no seed: %v", err)
	}
	if seed.Stat.Name != "user-data" {
		t.Fatalf("the seed's stat is %+v", seed.Stat)
	}

	// The container is running, and it is not privileged.
	running, err := opened.ContainerInspect(ctx, instance.ID, client.ContainerInspectOptions{})
	if err != nil {
		t.Fatalf("reading the machine: %v", err)
	}
	if running.Container.State == nil || !running.Container.State.Running {
		t.Fatalf("the machine is not running: %+v", running.Container.State)
	}
	if running.Container.HostConfig.Privileged {
		t.Fatal("the machine is privileged")
	}
	if running.Container.HostConfig.CgroupnsMode != container.CgroupnsModePrivate {
		t.Fatalf("the machine's cgroup namespace is %q", running.Container.HostConfig.CgroupnsMode)
	}

	// It lists by the labels the provisioner set.
	resources, err := provisioner.List(ctx, sdk.ListRequest{Engine: localEngine(), Kind: KindMachine, Entry: "lab-docker/container"})
	if err != nil {
		t.Fatalf("listing: %v", err)
	}
	found := false
	for _, resource := range resources {
		if resource.Name == name {
			found = true
			if resource.Address == "" || resource.Labels[LabelEntry] != "lab-docker/container" {
				t.Fatalf("the resource is %+v", resource)
			}
		}
	}
	if !found {
		t.Fatalf("the machine is not in %+v", resources)
	}

	// Stop, start, and remove.
	if err := provisioner.Stop(ctx, instance); err != nil {
		t.Fatalf("stopping the machine: %v", err)
	}
	stopped, err := opened.ContainerInspect(ctx, instance.ID, client.ContainerInspectOptions{})
	if err != nil {
		t.Fatalf("reading the machine: %v", err)
	}
	if stopped.Container.State == nil || stopped.Container.State.Running {
		t.Fatal("the machine is still running")
	}

	started, err := provisioner.Start(ctx, instance)
	if err != nil {
		t.Fatalf("starting the machine: %v", err)
	}
	if started.Address == "" {
		t.Fatal("the started machine has no address")
	}

	got, err := provisioner.Get(ctx, started)
	if err != nil {
		t.Fatalf("reading the machine again: %v", err)
	}
	if got.Address == "" {
		t.Fatal("the machine has no address after Get")
	}

	if err := provisioner.Remove(ctx, started); err != nil {
		t.Fatalf("removing the machine: %v", err)
	}
	if _, err := opened.ContainerInspect(ctx, instance.ID, client.ContainerInspectOptions{}); err == nil {
		t.Fatal("the machine's container is still there")
	}
	// Its volumes went with it.
	for _, volume := range []string{
		dockerName("rookery", "lab-docker/container", name, "disk"),
		dockerName("rookery", "lab-docker/container", name, "seed"),
	} {
		if _, err := opened.VolumeInspect(ctx, volume, client.VolumeInspectOptions{}); err == nil {
			t.Errorf("the volume %s is still there", volume)
		}
	}
}

// TestDeployment runs a compose project, reads its logs, stops it, starts it and
// removes it.
func TestDeployment(t *testing.T) {
	provisioner := provisioner(t)
	opened := engine(t)
	name := hostName("deployment")
	ctx := t.Context()

	args := mustMarshal(t, map[string]any{
		"project": name,
		"content": fmt.Sprintf(`
services:
  web:
    image: %s
    command: ["sh", "-c", "echo the web service is up; sleep 300"]
    environment:
      GREETING: hello
    labels:
      app: rookery-test
    ports: ["127.0.0.1::8080"]
volumes:
  data: {}
`, testImage()),
	})
	request := sdk.CreateRequest{Engine: localEngine(), Entry: "lab-docker/container", Kind: KindDeployment, Name: name, Args: args}
	instance, err := provisioner.Create(ctx, request)
	if err != nil {
		t.Fatalf("creating the deployment: %v", err)
	}
	t.Cleanup(func() {
		_ = provisioner.Remove(context.WithoutCancel(ctx), instance)
	})
	if instance.ID != dockerName(name) && instance.ID != name {
		t.Fatalf("the deployment's id is %q", instance.ID)
	}

	// Its container is running, with the labels and the environment compose
	// gave it.
	found, err := opened.ContainerList(ctx, client.ContainerListOptions{
		All:     true,
		Filters: client.Filters{}.Add("label", LabelName+"="+name),
	})
	if err != nil {
		t.Fatalf("finding the deployment: %v", err)
	}
	if len(found.Items) != 1 {
		t.Fatalf("the deployment has %d containers", len(found.Items))
	}
	web := found.Items[0]
	if web.State != container.StateRunning {
		t.Fatalf("the web service is %s", web.State)
	}
	if web.Labels[ComposeService] != "web" || web.Labels["app"] != "rookery-test" {
		t.Fatalf("the web service's labels are %v", web.Labels)
	}

	// The logs action carries what the service wrote.
	lines, err := collect(t, func(line func(string)) error {
		return provisioner.RunAction(ctx, sdk.ActionRequest{
			Engine:    localEngine(),
			Action:    "logs",
			Args:      mustMarshal(t, map[string]any{"name": name, "tail": 20}),
			EntryArgs: args,
		}, line)
	})
	if err != nil {
		t.Fatalf("reading the logs: %v", err)
	}
	joined := strings.Join(lines, "\n")
	if !strings.Contains(joined, "the web service is up") {
		t.Fatalf("the logs are %q", joined)
	}
	if !strings.Contains(joined, "web: ") {
		t.Fatalf("the logs do not say which service wrote them: %q", joined)
	}

	// Stop, list, start, remove.
	if err := provisioner.Stop(ctx, instance); err != nil {
		t.Fatalf("stopping the deployment: %v", err)
	}
	stopped, err := opened.ContainerInspect(ctx, web.ID, client.ContainerInspectOptions{})
	if err != nil {
		t.Fatalf("reading the service: %v", err)
	}
	if stopped.Container.State != nil && stopped.Container.State.Running {
		t.Fatal("the web service is still running")
	}

	resources, err := provisioner.List(ctx, sdk.ListRequest{Engine: localEngine(), Entry: "lab-docker/container"})
	if err != nil {
		t.Fatalf("listing: %v", err)
	}
	deployments := 0
	for _, resource := range resources {
		if resource.Kind == KindDeployment && resource.Name == name {
			deployments++
		}
	}
	if deployments != 1 {
		t.Fatalf("the deployments are %+v", resources)
	}

	if _, err := provisioner.Start(ctx, instance); err != nil {
		t.Fatalf("starting the deployment: %v", err)
	}
	if _, err := provisioner.Get(ctx, instance); err != nil {
		t.Fatalf("reading the deployment: %v", err)
	}

	if err := provisioner.Remove(ctx, instance); err != nil {
		t.Fatalf("removing the deployment: %v", err)
	}
	if _, err := opened.ContainerInspect(ctx, web.ID, client.ContainerInspectOptions{}); err == nil {
		t.Fatal("the web service is still there")
	}
}

// TestActions checks the actions that read the engine.
func TestActions(t *testing.T) {
	provisioner := provisioner(t)
	ctx := t.Context()
	args := mustMarshal(t, map[string]any{"image": testImage()})

	pulled, err := collect(t, func(line func(string)) error {
		return provisioner.RunAction(ctx, sdk.ActionRequest{
			Engine: localEngine(), Action: "pull", Args: mustMarshal(t, map[string]any{"image": testImage()}), EntryArgs: args,
		}, line)
	})
	if err != nil {
		t.Fatalf("pulling the image: %v", err)
	}
	if len(pulled) == 0 || !strings.Contains(strings.Join(pulled, "\n"), testImage()) {
		t.Fatalf("the pull is %v", pulled)
	}

	images, err := collect(t, func(line func(string)) error {
		return provisioner.RunAction(ctx, sdk.ActionRequest{Engine: localEngine(), Action: "images", EntryArgs: args}, line)
	})
	if err != nil {
		t.Fatalf("listing the images: %v", err)
	}
	if len(images) == 0 || !strings.Contains(strings.Join(images, "\n"), "alpine") {
		t.Fatalf("the images are %v", images)
	}

	if _, err := collect(t, func(line func(string)) error {
		return provisioner.RunAction(ctx, sdk.ActionRequest{Engine: localEngine(), Action: "volumes", EntryArgs: args}, line)
	}); err != nil {
		t.Fatalf("listing the volumes: %v", err)
	}

	if err := provisioner.RunAction(ctx, sdk.ActionRequest{Engine: localEngine(), Action: "nonsense", EntryArgs: args}, func(string) {}); err == nil {
		t.Fatal("an action that does not exist worked")
	}
}

// TestPrune removes what a stopped deployment left and nothing uses.
func TestPrune(t *testing.T) {
	provisioner := provisioner(t)
	opened := engine(t)
	ctx := t.Context()
	name := hostName("prune")

	args := mustMarshal(t, map[string]any{
		"project": name,
		"content": fmt.Sprintf(`
services:
  web:
    image: %s
    command: ["sh", "-c", "sleep 300"]
volumes:
  data: {}
`, testImage()),
	})
	instance, err := provisioner.Create(ctx, sdk.CreateRequest{
		Engine: localEngine(), Entry: "lab-docker/container", Kind: KindDeployment, Name: name, Args: args,
	})
	if err != nil {
		t.Fatalf("creating the deployment: %v", err)
	}
	t.Cleanup(func() {
		_ = provisioner.Remove(context.WithoutCancel(ctx), instance)
	})
	if err := provisioner.Stop(ctx, instance); err != nil {
		t.Fatalf("stopping the deployment: %v", err)
	}

	if _, err := collect(t, func(line func(string)) error {
		return provisioner.RunAction(ctx, sdk.ActionRequest{Engine: localEngine(), Action: "prune"}, line)
	}); err != nil {
		t.Fatalf("pruning: %v", err)
	}

	// The stopped deployment's container is gone, so its volume is free and gone
	// too.
	found, err := opened.ContainerList(ctx, client.ContainerListOptions{
		All:     true,
		Filters: client.Filters{}.Add("label", LabelName+"="+name),
	})
	if err != nil {
		t.Fatalf("finding the deployment: %v", err)
	}
	if len(found.Items) != 0 {
		t.Fatal("the pruned deployment's container is still there")
	}
	if _, err := opened.VolumeInspect(ctx, dockerName(name, "data"), client.VolumeInspectOptions{}); err == nil {
		t.Fatal("the pruned deployment's volume is still there")
	}
}

// collect runs a stream of lines into a slice.
func collect(t *testing.T, run func(line func(string)) error) ([]string, error) {
	t.Helper()

	lines := []string{}
	err := run(func(line string) { lines = append(lines, line) })
	return lines, err
}

// testImage is the small image the deployment tests use.
func testImage() string {
	if image := os.Getenv("ROOKERY_TEST_IMAGE"); image != "" {
		return image
	}
	return "alpine:3.20"
}

// contains reports whether a list holds a value.
func contains(values []string, wanted string) bool {
	for _, value := range values {
		if value == wanted {
			return true
		}
	}
	return false
}
