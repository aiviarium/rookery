package provision

import (
	"context"
	"fmt"
	"strings"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/pkg/sdk"
)

// emptyArguments is the CBOR map an entry with no arguments carries: an empty
// map, never NULL, which a provisioner decodes like any other.
var emptyArguments = []byte{0xa0}

// Service is ProvisionerService, the edge's view of the core's provisioners.
type Service struct {
	provisioners *Provisioners
	jobs         *jobs.Service
	checker      *access.Checker
	seeds        Seeds
}

// Seeds renders what a created machine's cloud-init starts with. The deployment
// implements it from the key ring and the tailnet; a test gives a fixed one.
type Seeds interface {
	MachineSeed(ctx context.Context, host string) (Seed, error)
}

// NewService returns ProvisionerService.
func NewService(provisioners *Provisioners, checker *access.Checker, seeds Seeds) *Service {
	return &Service{provisioners: provisioners, checker: checker, seeds: seeds}
}

// RunActionsAsJobs uses the job runner for the production edge.
func (s *Service) RunActionsAsJobs(registry *jobs.Registry, service *jobs.Service) error {
	s.jobs = service
	return registry.Handle("provisioner.action", jobs.HandlerFunc(func(ctx context.Context, job jobs.Job, log *jobs.Log) ([]byte, error) {
		var action actionArguments
		if err := cborx.Unmarshal(job.Args, &action); err != nil {
			return nil, fmt.Errorf("%w: invalid provisioner action arguments: %w", access.ErrInvalid, err)
		}
		name, ok := strings.CutPrefix(job.Resource, "provisioner://")
		if !ok || name == "" || action.Action == "" {
			return nil, fmt.Errorf("%w: an action requires a provisioner and action name", access.ErrInvalid)
		}
		err := s.provisioners.RunAction(ctx, name, action.Action, action.Args, func(line string) { //nolint:contextcheck // the log owns its job context
			_ = log.WriteLine(line) //nolint:contextcheck // the log owns its job context
		})
		return nil, err
	}))
}

// RunMachinesAsJobs uses the job runner for the machines a provisioner makes:
// creating one and removing one are the host.create and host.remove jobs.
func (s *Service) RunMachinesAsJobs(registry *jobs.Registry, service *jobs.Service) error {
	s.jobs = service
	if err := registry.Handle("host.create", jobs.HandlerFunc(func(ctx context.Context, job jobs.Job, log *jobs.Log) ([]byte, error) {
		var machine machineArguments
		if err := cborx.Unmarshal(job.Args, &machine); err != nil {
			return nil, fmt.Errorf("%w: invalid machine arguments: %w", access.ErrInvalid, err)
		}
		if machine.Host == "" || machine.Provisioner == "" {
			return nil, fmt.Errorf("%w: a machine needs a host and a provisioner", access.ErrInvalid)
		}
		created, err := s.createMachine(ctx, machine, func(line string) { //nolint:contextcheck // the log owns its job context
			_ = log.WriteLine(line) //nolint:contextcheck // the log owns its job context
		})
		if err != nil {
			return nil, err
		}
		return cborx.Marshal(created)
	})); err != nil {
		return err
	}
	return registry.Handle("host.remove", jobs.HandlerFunc(func(ctx context.Context, job jobs.Job, log *jobs.Log) ([]byte, error) {
		var machine machineArguments
		if err := cborx.Unmarshal(job.Args, &machine); err != nil {
			return nil, fmt.Errorf("%w: invalid machine arguments: %w", access.ErrInvalid, err)
		}
		if machine.Host == "" {
			return nil, fmt.Errorf("%w: a removal needs a host", access.ErrInvalid)
		}
		return nil, s.provisioners.RemoveMachine(ctx, machine.Host, func(line string) { //nolint:contextcheck // the log owns its job context
			_ = log.WriteLine(line) //nolint:contextcheck // the log owns its job context
		})
	}))
}

type actionArguments struct {
	Action string `cbor:"action"`
	Args   []byte `cbor:"args"`
}

// machineArguments is what a host.create or host.remove job works from.
type machineArguments struct {
	Host        string `cbor:"host"`
	Provisioner string `cbor:"provisioner"`
	Args        []byte `cbor:"args"`
	Type        string `cbor:"type"`
	CreatedBy   string `cbor:"created_by"`
}

// ListTypes returns every provisioner type the installed plugins declare, with
// what each says it takes. A type whose executable cannot be asked still lists,
// with the kinds its manifest declares and the reason it could not be asked.
func (s *Service) ListTypes(ctx context.Context, _ *connect.Request[rookeryv1.ProvisionerServiceListTypesRequest]) (*connect.Response[rookeryv1.ProvisionerServiceListTypesResponse], error) {
	types, err := s.provisioners.Types(ctx)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := make([]*rookeryv1.ProvisionerType, 0, len(types))
	for _, typ := range types {
		message := &rookeryv1.ProvisionerType{
			Id:     typ.ID,
			Plugin: typ.Plugin,
			Label:  typ.Label,
		}
		for _, kind := range typ.KindList() {
			message.Kinds = append(message.Kinds, &rookeryv1.ProvisionerKind{Id: kind.ID, Label: kind.Label, Machine: kind.Machine})
		}
		if description, err := s.provisioners.Describe(ctx, typ); err != nil {
			message.Error = err.Error()
		} else {
			describe(message, description)
		}
		out = append(out, message)
	}
	return connect.NewResponse(&rookeryv1.ProvisionerServiceListTypesResponse{Types: out}), nil
}

// describe writes a description into a type's message.
func describe(message *rookeryv1.ProvisionerType, description sdk.Description) {
	if description.Type != "" {
		message.Id = description.Type
	}
	if description.Label != "" {
		message.Label = description.Label
	}
	if len(description.Kinds) > 0 {
		message.Kinds = message.Kinds[:0]
		for _, kind := range description.Kinds {
			message.Kinds = append(message.Kinds, &rookeryv1.ProvisionerKind{Id: kind.ID, Label: kind.Label, Machine: kind.Machine})
		}
	}
	for _, argument := range description.Arguments {
		message.Arguments = append(message.Arguments, argumentMessage(argument))
	}
	message.Secrets = description.Secrets
	for _, size := range description.Sizes {
		message.Sizes = append(message.Sizes, &rookeryv1.ProvisionerSize{
			Id: size.ID, Label: size.Label, Cpus: size.CPUs, MemoryMb: size.MemoryMB, DiskGb: size.DiskGB,
		})
	}
	for _, image := range description.Images {
		message.Images = append(message.Images, &rookeryv1.ProvisionerImage{
			Id: image.ID, Label: image.Label, Os: image.OS, Release: image.Release, Architecture: image.Architecture,
		})
	}
	for _, action := range description.Actions {
		out := &rookeryv1.ProvisionerAction{
			Name: action.Name, Label: action.Label, Group: action.Group, Destructive: action.Destructive,
		}
		for _, argument := range action.Arguments {
			out.Arguments = append(out.Arguments, argumentMessage(argument))
		}
		message.Actions = append(message.Actions, out)
	}
	message.Node = description.Node
	message.Local = description.Local
}

func argumentMessage(argument sdk.Argument) *rookeryv1.ProvisionerArgument {
	return &rookeryv1.ProvisionerArgument{
		Name:        argument.Name,
		Label:       argument.Label,
		Type:        argument.Type,
		Required:    argument.Required,
		DefaultCbor: argument.Default,
		Choices:     argument.Choices,
		Help:        argument.Help,
		Secret:      argument.Secret,
	}
}

// List returns a page of entries the caller may view.
func (s *Service) List(ctx context.Context, request *connect.Request[rookeryv1.ProvisionerServiceListRequest]) (*connect.Response[rookeryv1.ProvisionerServiceListResponse], error) {
	principal := access.MustPrincipal(ctx)
	limit := int(request.Msg.GetLimit())
	entries, err := s.provisioners.Entries().List(ctx, principal, request.Msg.GetCursor(), limit)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := make([]*rookeryv1.ProvisionerEntry, 0, len(entries))
	for _, entry := range entries {
		out = append(out, entryMessage(entry))
	}
	next := ""
	if len(entries) == int(clampLimit(limit)) {
		next = entries[len(entries)-1].Name
	}
	return connect.NewResponse(&rookeryv1.ProvisionerServiceListResponse{Provisioners: out, NextCursor: next}), nil
}

// Get returns one entry.
func (s *Service) Get(ctx context.Context, request *connect.Request[rookeryv1.ProvisionerServiceGetRequest]) (*connect.Response[rookeryv1.ProvisionerServiceGetResponse], error) {
	entry, err := s.provisioners.Entries().ByName(ctx, request.Msg.GetProvisioner())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ProvisionerServiceGetResponse{Provisioner: entryMessage(entry)}), nil
}

// Add makes an entry, refusing arguments the type does not take.
func (s *Service) Add(ctx context.Context, request *connect.Request[rookeryv1.ProvisionerServiceAddRequest]) (*connect.Response[rookeryv1.ProvisionerServiceAddResponse], error) {
	typ, err := s.provisioners.Type(ctx, request.Msg.GetType())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	args := request.Msg.GetArgsCbor()
	if len(args) == 0 {
		args = emptyArguments
	}
	if err := s.checkArguments(ctx, typ, args); err != nil {
		return nil, access.ToConnect(err)
	}
	entry, err := s.provisioners.Entries().Add(ctx, Entry{
		Name:   request.Msg.GetName(),
		Type:   typ.ID,
		Plugin: typ.Plugin,
		Args:   args,
		Node:   request.Msg.GetNode(),
		Remote: request.Msg.GetRemote(),
	})
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ProvisionerServiceAddResponse{Provisioner: entryMessage(entry)}), nil
}

// Update changes an entry's arguments or where its engine is.
func (s *Service) Update(ctx context.Context, request *connect.Request[rookeryv1.ProvisionerServiceUpdateRequest]) (*connect.Response[rookeryv1.ProvisionerServiceUpdateResponse], error) {
	name := request.Msg.GetProvisioner()
	entry, err := s.provisioners.Entries().ByName(ctx, name)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	args := request.Msg.GetArgsCbor()
	if len(args) == 0 {
		args = entry.Args
	}
	if typ, typeErr := s.provisioners.Type(ctx, entry.Type); typeErr == nil {
		if err := s.checkArguments(ctx, typ, args); err != nil {
			return nil, access.ToConnect(err)
		}
	}
	updated, err := s.provisioners.Entries().Update(ctx, name, args, request.Msg.GetNode(), request.Msg.GetRemote())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ProvisionerServiceUpdateResponse{Provisioner: entryMessage(updated)}), nil
}

// Remove removes an entry.
func (s *Service) Remove(ctx context.Context, request *connect.Request[rookeryv1.ProvisionerServiceRemoveRequest]) (*connect.Response[rookeryv1.ProvisionerServiceRemoveResponse], error) {
	if err := s.provisioners.Entries().Remove(ctx, request.Msg.GetProvisioner()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ProvisionerServiceRemoveResponse{}), nil
}

// Status says whether an entry's engine answers.
func (s *Service) Status(ctx context.Context, request *connect.Request[rookeryv1.ProvisionerServiceStatusRequest]) (*connect.Response[rookeryv1.ProvisionerServiceStatusResponse], error) {
	status, err := s.provisioners.Status(ctx, request.Msg.GetProvisioner())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ProvisionerServiceStatusResponse{Up: status.Up, Message: status.Message}), nil
}

// ListResources returns a page of what an entry made.
func (s *Service) ListResources(ctx context.Context, request *connect.Request[rookeryv1.ProvisionerServiceListResourcesRequest]) (*connect.Response[rookeryv1.ProvisionerServiceListResourcesResponse], error) {
	if _, err := s.provisioners.Entries().ByName(ctx, request.Msg.GetProvisioner()); err != nil {
		return nil, access.ToConnect(err)
	}
	limit := int(request.Msg.GetLimit())
	resources, err := s.provisioners.Resources().List(ctx, request.Msg.GetProvisioner(), request.Msg.GetKind(), request.Msg.GetCursor(), limit)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := make([]*rookeryv1.ProvisionerResource, 0, len(resources))
	for _, resource := range resources {
		out = append(out, resourceMessage(resource))
	}
	next := ""
	if len(resources) == int(clampLimit(limit)) {
		next = resources[len(resources)-1].ID
	}
	return connect.NewResponse(&rookeryv1.ProvisionerServiceListResourcesResponse{Resources: out, NextCursor: next}), nil
}

// RunAction runs one of the type's actions, streaming its output.
func (s *Service) RunAction(ctx context.Context, request *connect.Request[rookeryv1.ProvisionerServiceRunActionRequest], stream *connect.ServerStream[rookeryv1.ProvisionerServiceRunActionResponse]) error {
	if s.jobs != nil {
		if _, err := s.provisioners.Entries().ByName(ctx, request.Msg.GetProvisioner()); err != nil {
			return access.ToConnect(err)
		}
		if request.Msg.GetAction() == "" {
			return access.ToConnect(fmt.Errorf("%w: a provisioner action needs a name", access.ErrInvalid))
		}
		args, err := cborx.Marshal(actionArguments{Action: request.Msg.GetAction(), Args: request.Msg.GetArgsCbor()})
		if err != nil {
			return access.ToConnect(err)
		}
		job, err := s.jobs.Enqueue(ctx, jobs.EnqueueRequest{
			Kind: "provisioner.action", Args: args, Resource: access.ProvisionerURI(request.Msg.GetProvisioner()),
		})
		if err != nil {
			return access.ToConnect(err)
		}
		return stream.Send(&rookeryv1.ProvisionerServiceRunActionResponse{Job: job.ID})
	}
	err := s.provisioners.RunAction(ctx, request.Msg.GetProvisioner(), request.Msg.GetAction(), request.Msg.GetArgsCbor(), func(line string) { //nolint:contextcheck // the log owns its job context
		// A failed send means the caller is gone; the call's context ends and
		// the runner stops the provisioner.
		_ = stream.Send(&rookeryv1.ProvisionerServiceRunActionResponse{Line: line})
	})
	if err != nil {
		return access.ToConnect(err)
	}
	return nil
}

// CreateMachine makes a host's machine through an entry. With a job runner it
// queues the host.create job and returns its id; without one, as in a test, it
// runs the call and returns the resource.
func (s *Service) CreateMachine(ctx context.Context, request *connect.Request[rookeryv1.ProvisionerServiceCreateMachineRequest]) (*connect.Response[rookeryv1.ProvisionerServiceCreateMachineResponse], error) {
	principal, ok := access.PrincipalFrom(ctx)
	if !ok {
		return nil, access.ToConnect(access.ErrUnauthenticated)
	}
	machine := machineArguments{
		Host:        strings.TrimSpace(request.Msg.GetHost()),
		Provisioner: request.Msg.GetProvisioner(),
		Args:        request.Msg.GetArgsCbor(),
		Type:        machineType(request.Msg.GetType()),
	}
	if principal.Kind == access.KindUser {
		machine.CreatedBy = principal.ID
	}
	if machine.Host == "" || machine.Provisioner == "" {
		return nil, access.ToConnect(fmt.Errorf("%w: a machine needs a host and a provisioner", access.ErrInvalid))
	}
	if s.jobs != nil {
		// The job is the host's, so the caller needs manage on it to follow and
		// cancel it. Creating a host is what grants that, before the job runs.
		if principal.Kind == access.KindUser {
			if err := s.checker.GrantUser(ctx, principal.ID, access.HostURI(machine.Host), access.Manage); err != nil {
				return nil, access.ToConnect(err)
			}
		}
		encoded, err := cborx.Marshal(machine)
		if err != nil {
			return nil, access.ToConnect(err)
		}
		job, err := s.jobs.Enqueue(ctx, jobs.EnqueueRequest{
			Kind: "host.create", Args: encoded, Resource: access.HostURI(machine.Host),
		})
		if err != nil {
			return nil, access.ToConnect(err)
		}
		return connect.NewResponse(&rookeryv1.ProvisionerServiceCreateMachineResponse{Job: job.ID}), nil
	}
	created, err := s.createMachine(ctx, machine, nil)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ProvisionerServiceCreateMachineResponse{Machine: resourceMessage(created)}), nil
}

// ListMachines is a page of the machines the caller may view, as the hosts they
// are.
func (s *Service) ListMachines(ctx context.Context, request *connect.Request[rookeryv1.ProvisionerServiceListMachinesRequest]) (*connect.Response[rookeryv1.ProvisionerServiceListMachinesResponse], error) {
	principal := access.MustPrincipal(ctx)
	limit := int(request.Msg.GetLimit())
	resources, err := s.provisioners.Resources().List(ctx, request.Msg.GetProvisioner(), KindMachine, request.Msg.GetCursor(), limit)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := make([]*rookeryv1.ProvisionerResource, 0, len(resources))
	for _, resource := range resources {
		level, err := s.checker.Level(ctx, principal, access.HostURI(resource.Name))
		if err != nil {
			return nil, access.ToConnect(err)
		}
		if !access.Satisfies(level, access.View) {
			continue
		}
		out = append(out, resourceMessage(resource))
	}
	next := ""
	if len(resources) == int(clampLimit(limit)) {
		next = resources[len(resources)-1].ID
	}
	return connect.NewResponse(&rookeryv1.ProvisionerServiceListMachinesResponse{Machines: out, NextCursor: next}), nil
}

// RemoveMachine removes a host's machine and its host. With a job runner it
// queues the host.remove job and returns its id.
func (s *Service) RemoveMachine(ctx context.Context, request *connect.Request[rookeryv1.ProvisionerServiceRemoveMachineRequest]) (*connect.Response[rookeryv1.ProvisionerServiceRemoveMachineResponse], error) {
	host := strings.TrimSpace(request.Msg.GetHost())
	if host == "" {
		return nil, access.ToConnect(fmt.Errorf("%w: a removal needs a host", access.ErrInvalid))
	}
	if s.jobs != nil {
		encoded, err := cborx.Marshal(machineArguments{Host: host})
		if err != nil {
			return nil, access.ToConnect(err)
		}
		job, err := s.jobs.Enqueue(ctx, jobs.EnqueueRequest{
			Kind: "host.remove", Args: encoded, Resource: access.HostURI(host),
		})
		if err != nil {
			return nil, access.ToConnect(err)
		}
		return connect.NewResponse(&rookeryv1.ProvisionerServiceRemoveMachineResponse{Job: job.ID}), nil
	}
	if err := s.provisioners.RemoveMachine(ctx, host, nil); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ProvisionerServiceRemoveMachineResponse{}), nil
}

// createMachine renders the machine's seed and makes it through its entry.
func (s *Service) createMachine(ctx context.Context, machine machineArguments, line func(string)) (Resource, error) {
	if s.seeds == nil {
		return Resource{}, fmt.Errorf("%w: a machine needs the key ring and the tailnet", access.ErrUnavailable)
	}
	seed, err := s.seeds.MachineSeed(ctx, machine.Host)
	if err != nil {
		return Resource{}, err
	}
	return s.provisioners.CreateMachine(ctx, MachineRequest{
		Host:      machine.Host,
		Entry:     machine.Provisioner,
		Args:      machine.Args,
		Seed:      seed,
		Type:      machine.Type,
		CreatedBy: machine.CreatedBy,
		Line:      line,
	})
}

// machineType is a host type as the profile names it, empty for the default.
func machineType(t rookeryv1.HostType) string {
	switch t {
	case rookeryv1.HostType_HOST_TYPE_MANAGED:
		return "managed"
	case rookeryv1.HostType_HOST_TYPE_WORKSTATION:
		return "workstation"
	default:
		return ""
	}
}

// checkArguments refuses arguments the type does not take, and ones a required
// argument is missing from. A type that cannot be asked is not checked: a broken
// executable should not stop an entry being fixed.
func (s *Service) checkArguments(ctx context.Context, typ Type, args []byte) error {
	description, err := s.provisioners.Describe(ctx, typ)
	if err != nil {
		return nil
	}
	given := map[string]any{}
	if len(args) > 0 {
		if err := cborx.Unmarshal(args, &given); err != nil {
			return fmt.Errorf("%w: the arguments are not a CBOR map: %w", access.ErrInvalid, err)
		}
	}
	declared := make(map[string]sdk.Argument, len(description.Arguments))
	for _, argument := range description.Arguments {
		declared[argument.Name] = argument
	}
	for name := range given {
		if _, ok := declared[name]; !ok {
			return fmt.Errorf("%w: %s takes no argument %q", access.ErrInvalid, typ.ID, name)
		}
	}
	for _, argument := range description.Arguments {
		if !argument.Required {
			continue
		}
		if _, ok := given[argument.Name]; !ok {
			return fmt.Errorf("%w: %s needs %q", access.ErrInvalid, typ.ID, argument.Name)
		}
	}
	return nil
}

func entryMessage(entry Entry) *rookeryv1.ProvisionerEntry {
	out := &rookeryv1.ProvisionerEntry{
		Name:     entry.Name,
		Type:     entry.Type,
		Plugin:   entry.Plugin,
		ArgsCbor: entry.Args,
		Node:     entry.Node,
		Remote:   entry.Remote,
	}
	if !entry.CreatedAt.IsZero() {
		out.CreatedAt = timestamppb.New(entry.CreatedAt)
	}
	if !entry.UpdatedAt.IsZero() {
		out.UpdatedAt = timestamppb.New(entry.UpdatedAt)
	}
	return out
}

func resourceMessage(resource Resource) *rookeryv1.ProvisionerResource {
	out := &rookeryv1.ProvisionerResource{
		Id:          resource.ID,
		Kind:        resource.Kind,
		Name:        resource.Name,
		Provisioner: resource.Provisioner,
		StateCbor:   resource.State,
		Address:     resource.Address,
	}
	if !resource.CreatedAt.IsZero() {
		out.CreatedAt = timestamppb.New(resource.CreatedAt)
	}
	if !resource.UpdatedAt.IsZero() {
		out.UpdatedAt = timestamppb.New(resource.UpdatedAt)
	}
	return out
}
