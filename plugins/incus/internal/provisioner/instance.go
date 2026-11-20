package provisioner

import (
	"context"
	"fmt"
	"net/http"
	"strconv"
	"strings"

	incus "github.com/lxc/incus/v6/client"
	"github.com/lxc/incus/v6/shared/api"

	"github.com/aiviarium/rookery/pkg/sdk"
)

// The labels every machine carries, in its own configuration: user.rookery.*, as
// Incus keeps a user's keys.
const (
	// LabelPrefix is the start of every key the provisioner sets.
	LabelPrefix = "user.rookery."

	// LabelEntry is the provisioner entry that made the machine.
	LabelEntry = LabelPrefix + "entry"

	// LabelKind is the resource kind: machine.
	LabelKind = LabelPrefix + "kind"

	// LabelName is what the machine is called in Rookery.
	LabelName = LabelPrefix + "name"

	// LabelType is the provisioner type: incus/vm or incus/container.
	LabelType = LabelPrefix + "type"
)

// machineState is what the provisioner keeps to reach a machine again.
type machineState struct {
	Entry   string `cbor:"entry"`
	Project string `cbor:"project"`
	Type    string `cbor:"type"`
	Name    string `cbor:"name"`
}

// createMachine makes a machine: an instance from a cloud image, with the
// rendered seed as its cloud-init user-data, its sizes, its profile and project,
// and the provisioner's labels.
func createMachine(ctx context.Context, server incus.InstanceServer, instanceType string, arguments arguments, request sdk.CreateRequest) (sdk.Instance, error) {
	if request.Name == "" {
		return sdk.Instance{}, fmt.Errorf("a machine needs a name")
	}
	if !validName(request.Name) {
		return sdk.Instance{}, fmt.Errorf("%q is not a name an Incus instance can have", request.Name)
	}
	if strings.TrimSpace(request.Seed) == "" {
		return sdk.Instance{}, fmt.Errorf("a machine needs its cloud-init seed")
	}
	chosen, err := arguments.size()
	if err != nil {
		return sdk.Instance{}, err
	}
	server = projectServer(server, arguments.Project)

	source, alias, protocol := arguments.image()
	config := api.ConfigMap{
		"cloud-init.user-data": request.Seed,
		"limits.cpu":           strconv.FormatUint(uint64(chosen.cpus), 10),
		"limits.memory":        strconv.FormatUint(chosen.memoryMB, 10) + "MB",
		LabelEntry:             request.Entry,
		LabelKind:              KindMachine,
		LabelName:              request.Name,
		LabelType:              instanceType,
	}
	devices := api.DevicesMap{
		"root": map[string]string{
			"type": "disk",
			"pool": arguments.Pool,
			"path": "/",
			"size": strconv.FormatUint(chosen.diskGB, 10) + "GB",
		},
	}
	post := api.InstancesPost{
		InstancePut: api.InstancePut{
			Config:      config,
			Devices:     devices,
			Profiles:    []string{arguments.Profile},
			Description: "Rookery machine " + request.Name,
		},
		Name: request.Name,
		Type: instanceTypeOf(instanceType),
	}
	post.Source = api.InstanceSource{
		Type:     "image",
		Server:   source,
		Protocol: protocol,
		Alias:    alias,
	}

	operation, err := server.CreateInstance(post)
	if err != nil {
		return sdk.Instance{}, fmt.Errorf("creating the machine %s: %w", request.Name, err)
	}
	if err := operation.Wait(); err != nil {
		return sdk.Instance{}, fmt.Errorf("creating the machine %s: %w", request.Name, err)
	}
	if err := setState(ctx, server, request.Name, "start"); err != nil {
		return sdk.Instance{}, err
	}
	return getMachine(ctx, server, sdk.Instance{
		Kind:  KindMachine,
		Name:  request.Name,
		ID:    request.Name,
		State: mustState(machineState{Entry: request.Entry, Project: arguments.Project, Type: instanceType, Name: request.Name}),
	})
}

// getMachine reports a machine's state and address.
func getMachine(ctx context.Context, server incus.InstanceServer, instance sdk.Instance) (sdk.Instance, error) {
	state, err := machineStateOf(instance)
	if err != nil {
		return sdk.Instance{}, err
	}
	server = projectServer(server, state.Project)
	found, _, err := server.GetInstance(state.Name)
	if err != nil {
		return sdk.Instance{}, fmt.Errorf("reading the machine %s: %w", instance.Name, err)
	}
	address, err := addressOf(ctx, server, state.Name)
	if err != nil {
		return sdk.Instance{}, err
	}
	instance.Address = address
	instance.ID = found.Name
	instance.State = mustState(state)
	return instance, nil
}

// startMachine starts a stopped machine.
func startMachine(ctx context.Context, server incus.InstanceServer, instance sdk.Instance) (sdk.Instance, error) {
	state, err := machineStateOf(instance)
	if err != nil {
		return sdk.Instance{}, err
	}
	server = projectServer(server, state.Project)
	if err := setState(ctx, server, state.Name, "start"); err != nil {
		return sdk.Instance{}, err
	}
	return getMachine(ctx, server, instance)
}

// stopMachine stops a machine and keeps it.
func stopMachine(ctx context.Context, server incus.InstanceServer, instance sdk.Instance) error {
	state, err := machineStateOf(instance)
	if err != nil {
		return err
	}
	server = projectServer(server, state.Project)
	return setState(ctx, server, state.Name, "stop")
}

// removeMachine removes a machine and its disk.
func removeMachine(_ context.Context, server incus.InstanceServer, instance sdk.Instance) error {
	state, err := machineStateOf(instance)
	if err != nil {
		return err
	}
	server = projectServer(server, state.Project)
	// A running instance is stopped first, forcefully: a machine is disposable
	// and its disk goes with it.
	current, _, err := server.GetInstance(state.Name)
	if err != nil {
		if api.StatusErrorCheck(err, http.StatusNotFound) {
			return nil
		}
		return fmt.Errorf("reading the machine %s: %w", instance.Name, err)
	}
	if current.StatusCode == api.Running {
		operation, err := server.UpdateInstanceState(state.Name, api.InstanceStatePut{
			Action:  "stop",
			Force:   true,
			Timeout: 60,
		}, "")
		if err != nil {
			return fmt.Errorf("stopping the machine %s: %w", instance.Name, err)
		}
		if err := operation.Wait(); err != nil {
			return fmt.Errorf("stopping the machine %s: %w", instance.Name, err)
		}
	}
	operation, err := server.DeleteInstance(state.Name)
	if err != nil {
		if api.StatusErrorCheck(err, http.StatusNotFound) {
			return nil
		}
		return fmt.Errorf("removing the machine %s: %w", instance.Name, err)
	}
	if err := operation.Wait(); err != nil {
		return fmt.Errorf("removing the machine %s: %w", instance.Name, err)
	}
	return nil
}

// listMachines is the machines one entry made, of one type, matched by the
// labels the provisioner set.
func listMachines(ctx context.Context, server incus.InstanceServer, instanceType, entry, project string) ([]sdk.Resource, error) {
	server = projectServer(server, project)
	found, err := server.GetInstances(instanceTypeOf(instanceType))
	if err != nil {
		return nil, fmt.Errorf("listing Incus's instances: %w", err)
	}
	resources := make([]sdk.Resource, 0, len(found))
	for _, instance := range found {
		if instance.Config[LabelEntry] != entry || instance.Config[LabelType] != instanceType {
			continue
		}
		name := instance.Config[LabelName]
		if name == "" {
			name = instance.Name
		}
		state := machineState{Entry: entry, Project: project, Type: instanceType, Name: instance.Name}
		address := ""
		if instance.StatusCode == api.Running {
			address, _ = addressOf(ctx, server, instance.Name)
		}
		resources = append(resources, sdk.Resource{
			Kind:    KindMachine,
			Name:    name,
			ID:      instance.Name,
			Address: address,
			State:   mustState(state),
			Labels:  labelsOf(instance),
		})
	}
	return resources, nil
}

// labelsOf is the machines's own labels, for a listing.
func labelsOf(instance api.Instance) map[string]string {
	labels := map[string]string{}
	for key, value := range instance.Config {
		if strings.HasPrefix(key, LabelPrefix) {
			labels[key] = value
		}
	}
	return labels
}

// setState starts, stops or restarts a machine.
func setState(_ context.Context, server incus.InstanceServer, name, action string) error {
	operation, err := server.UpdateInstanceState(name, api.InstanceStatePut{
		Action:  action,
		Timeout: 300,
	}, "")
	if err != nil {
		return fmt.Errorf("%s: %w", action, err)
	}
	if err := operation.Wait(); err != nil {
		return fmt.Errorf("%s: %w", action, err)
	}
	return nil
}

// addressOf is a machine's address on its first interface, once it is up.
func addressOf(_ context.Context, server incus.InstanceServer, name string) (string, error) {
	state, _, err := server.GetInstanceState(name)
	if err != nil {
		return "", fmt.Errorf("reading the machine's state: %w", err)
	}
	if state == nil {
		return "", nil
	}
	if settings, ok := state.Network["eth0"]; ok {
		for _, address := range settings.Addresses {
			if address.Family == "inet" && address.Scope == "global" {
				return address.Address, nil
			}
		}
	}
	return "", nil
}

// projectServer is the server with a project, when the entry names one.
func projectServer(server incus.InstanceServer, project string) incus.InstanceServer {
	if project == "" {
		return server
	}
	return server.UseProject(project)
}

// machineStateOf reads what the provisioner stored about a machine.
func machineStateOf(instance sdk.Instance) (machineState, error) {
	state := machineState{Name: instance.Name, Type: instance.Kind}
	if len(instance.State) == 0 {
		return state, nil
	}
	if err := sdk.Unmarshal(instance.State, &state); err != nil {
		return machineState{}, fmt.Errorf("the state is not a machine's: %w", err)
	}
	if state.Name == "" {
		state.Name = instance.Name
	}
	return state, nil
}

// instanceTypeOf turns the provisioner's type into Incus's.
func instanceTypeOf(instanceType string) api.InstanceType {
	if instanceType == TypeVM {
		return api.InstanceTypeVM
	}
	return api.InstanceTypeContainer
}

// mustState encodes a machine's state: it is small and of fixed shape, so an
// encoding error cannot happen.
func mustState(state machineState) []byte {
	encoded, err := sdk.Marshal(state)
	if err != nil {
		return nil
	}
	return encoded
}
