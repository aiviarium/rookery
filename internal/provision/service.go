package provision

import (
	"context"
	"fmt"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/pkg/sdk"
)

// emptyArguments is the CBOR map an entry with no arguments carries: an empty
// map, never NULL, which a provisioner decodes like any other.
var emptyArguments = []byte{0xa0}

// Service is ProvisionerService, the edge's view of the core's provisioners.
type Service struct {
	provisioners *Provisioners
}

// NewService returns ProvisionerService.
func NewService(provisioners *Provisioners) *Service {
	return &Service{provisioners: provisioners}
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
	err := s.provisioners.RunAction(ctx, request.Msg.GetProvisioner(), request.Msg.GetAction(), request.Msg.GetArgsCbor(), func(line string) {
		// A failed send means the caller is gone; the call's context ends and
		// the runner stops the provisioner.
		_ = stream.Send(&rookeryv1.ProvisionerServiceRunActionResponse{Line: line})
	})
	if err != nil {
		return access.ToConnect(err)
	}
	return nil
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
