package sdk

import (
	"context"
	"errors"
	"fmt"
	"net/http"

	"connectrpc.com/connect"

	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/plugin/v1/pluginv1connect"
)

// Engine is where an entry's engine is: the managed host whose engine the
// provisioner reaches through the server, or an address it reaches itself.
// Neither set is the server's own engine.
type Engine struct {
	// Node is the managed host whose engine the entry drives. Use the client's
	// NodeDialer for it.
	Node string

	// Remote is the address of an engine the provisioner reaches itself.
	Remote string
}

// Local reports whether the engine is the server's own.
func (e Engine) Local() bool { return e.Node == "" && e.Remote == "" }

// Kind is a resource kind a provisioner makes.
type Kind struct {
	// ID is the kind's name: machine, deployment, or the provisioner's own.
	ID    string
	Label string

	// Machine says the core knows this kind: one machine per host.
	Machine bool
}

// Argument is one argument an entry's type takes.
type Argument struct {
	Name     string
	Label    string
	Type     string
	Required bool

	// Default is the value when the entry leaves it out, encoded as CBOR.
	Default []byte

	// Choices are the values a select takes.
	Choices []string

	Help string

	// Secret says the value comes from the provisioner's secrets, not the
	// entry's arguments.
	Secret bool
}

// Size is a machine size a provisioner offers.
type Size struct {
	ID       string
	Label    string
	CPUs     uint32
	MemoryMB uint64
	DiskGB   uint64
}

// Image is a machine image a provisioner offers.
type Image struct {
	ID           string
	Label        string
	OS           string
	Release      string
	Architecture string
}

// Action is something an entry can be told to do with its engine.
type Action struct {
	Name        string
	Label       string
	Group       string
	Arguments   []Argument
	Destructive bool
}

// Description is what a provisioner says about itself.
type Description struct {
	// Type is the type's name, such as "docker/container".
	Type  string
	Label string

	Kinds     []Kind
	Arguments []Argument
	Secrets   []string
	Sizes     []Size
	Images    []Image
	Actions   []Action

	// Node says an entry runs against the engine on a managed host. Local says
	// the server's own engine is a node too.
	Node  bool
	Local bool
}

// CreateRequest is a resource to make.
type CreateRequest struct {
	// Engine is where the entry's engine is.
	Engine Engine

	// Entry is the provisioner entry's name.
	Entry string

	// Kind is the resource kind to make.
	Kind string

	// Name is what to call it: for a machine the host's name.
	Name string

	// Args are the entry's arguments, CBOR in Core Deterministic mode.
	Args []byte

	// Seed is the cloud-init user data a machine starts with.
	Seed string
}

// Instance is something a provisioner made.
type Instance struct {
	// Engine is where the entry's engine is.
	Engine  Engine
	Kind    string
	Name    string
	ID      string
	State   []byte
	Address string
}

// Status is whether an engine answers.
type Status struct {
	Up      bool
	Message string
}

// ListRequest asks what an entry made.
type ListRequest struct {
	// Engine is where the entry's engine is.
	Engine Engine

	// Kind narrows the list to one kind; empty lists every kind.
	Kind string

	// Args are the entry's arguments.
	Args []byte

	// Entry is the entry whose resources the list is.
	Entry string
}

// Resource is something an entry made, as List reports it.
type Resource struct {
	Kind    string
	Name    string
	ID      string
	Address string
	State   []byte
	Labels  map[string]string
}

// ActionRequest is one of a provisioner's actions to run.
type ActionRequest struct {
	// Engine is where the entry's engine is.
	Engine Engine

	Action string

	// Args are the action's arguments, CBOR in Core Deterministic mode.
	Args []byte

	// EntryArgs are the entry's arguments.
	EntryArgs []byte
}

// Provisioner is a provisioner's implementation. A plugin's provisioner program
// implements it and calls Run.
//
// Every method is one call: the server starts the program for the call and stops
// it afterwards, so nothing is kept between two of them that state does not hold.
type Provisioner interface {
	// Describe says what this provisioner makes and takes.
	Describe(ctx context.Context) (Description, error)

	// Status says whether the entry's engine answers.
	Status(ctx context.Context, engine Engine, args []byte) (Status, error)

	// Create makes one resource.
	Create(ctx context.Context, request CreateRequest) (Instance, error)

	// Get reports a resource's state and address.
	Get(ctx context.Context, instance Instance) (Instance, error)

	// Start starts a stopped resource.
	Start(ctx context.Context, instance Instance) (Instance, error)

	// Stop stops a resource and keeps it.
	Stop(ctx context.Context, instance Instance) error

	// Remove removes a resource and what it kept.
	Remove(ctx context.Context, instance Instance) error

	// List is what an entry made.
	List(ctx context.Context, request ListRequest) ([]Resource, error)

	// RunAction runs one of the actions, sending each line of its output to
	// line.
	RunAction(ctx context.Context, request ActionRequest, line func(string)) error
}

// ProvisionerClient is a provisioner's executable, as the server calls it.
type ProvisionerClient struct {
	rpc pluginv1connect.ProvisionerServiceClient
}

// NewProvisionerClient returns a client for a provisioner served at baseURL,
// speaking gRPC like Rookery's own processes do.
func NewProvisionerClient(httpClient *http.Client, baseURL string, options ...connect.ClientOption) *ProvisionerClient {
	all := make([]connect.ClientOption, 0, len(options)+1)
	all = append(all, connect.WithGRPC())
	all = append(all, options...)
	return &ProvisionerClient{rpc: pluginv1connect.NewProvisionerServiceClient(httpClient, baseURL, all...)}
}

// Describe asks what the provisioner makes and takes.
func (c *ProvisionerClient) Describe(ctx context.Context) (Description, error) {
	response, err := c.rpc.Describe(ctx, connect.NewRequest(&pluginv1.ProvisionerServiceDescribeRequest{}))
	if err != nil {
		return Description{}, err
	}
	return descriptionFromProto(response.Msg.GetDescription()), nil
}

// Status asks whether the engine answers.
func (c *ProvisionerClient) Status(ctx context.Context, engine Engine, args []byte) (Status, error) {
	response, err := c.rpc.Status(ctx, connect.NewRequest(&pluginv1.ProvisionerServiceStatusRequest{ArgsCbor: args, Engine: engineProto(engine)}))
	if err != nil {
		return Status{}, err
	}
	return Status{Up: response.Msg.GetUp(), Message: response.Msg.GetMessage()}, nil
}

// Create makes one resource.
func (c *ProvisionerClient) Create(ctx context.Context, request CreateRequest) (Instance, error) {
	response, err := c.rpc.Create(ctx, connect.NewRequest(&pluginv1.ProvisionerServiceCreateRequest{
		Entry:    request.Entry,
		Kind:     request.Kind,
		Name:     request.Name,
		ArgsCbor: request.Args,
		Seed:     request.Seed,
		Engine:   engineProto(request.Engine),
	}))
	if err != nil {
		return Instance{}, err
	}
	instance := Instance{Engine: request.Engine, Kind: request.Kind, Name: request.Name, ID: response.Msg.GetId(), Address: response.Msg.GetAddress(), State: response.Msg.GetStateCbor()}
	return instance, nil
}

// Get reports a resource's state and address.
func (c *ProvisionerClient) Get(ctx context.Context, instance Instance) (Instance, error) {
	response, err := c.rpc.Get(ctx, connect.NewRequest(&pluginv1.ProvisionerServiceGetRequest{Instance: instanceProto(instance)}))
	if err != nil {
		return Instance{}, err
	}
	instance.State = response.Msg.GetStateCbor()
	instance.Address = response.Msg.GetAddress()
	return instance, nil
}

// Start starts a stopped resource.
func (c *ProvisionerClient) Start(ctx context.Context, instance Instance) (Instance, error) {
	response, err := c.rpc.Start(ctx, connect.NewRequest(&pluginv1.ProvisionerServiceStartRequest{Instance: instanceProto(instance)}))
	if err != nil {
		return Instance{}, err
	}
	instance.State = response.Msg.GetStateCbor()
	instance.Address = response.Msg.GetAddress()
	return instance, nil
}

// Stop stops a resource and keeps it.
func (c *ProvisionerClient) Stop(ctx context.Context, instance Instance) error {
	_, err := c.rpc.Stop(ctx, connect.NewRequest(&pluginv1.ProvisionerServiceStopRequest{Instance: instanceProto(instance)}))
	return err
}

// Remove removes a resource and what it kept.
func (c *ProvisionerClient) Remove(ctx context.Context, instance Instance) error {
	_, err := c.rpc.Remove(ctx, connect.NewRequest(&pluginv1.ProvisionerServiceRemoveRequest{Instance: instanceProto(instance)}))
	return err
}

// List is what an entry made.
func (c *ProvisionerClient) List(ctx context.Context, request ListRequest) ([]Resource, error) {
	response, err := c.rpc.List(ctx, connect.NewRequest(&pluginv1.ProvisionerServiceListRequest{
		Kind:     request.Kind,
		ArgsCbor: request.Args,
		Engine:   engineProto(request.Engine),
		Entry:    request.Entry,
	}))
	if err != nil {
		return nil, err
	}
	resources := make([]Resource, 0, len(response.Msg.GetResources()))
	for _, resource := range response.Msg.GetResources() {
		resources = append(resources, Resource{
			Kind:    resource.GetKind(),
			Name:    resource.GetName(),
			ID:      resource.GetId(),
			Address: resource.GetAddress(),
			State:   resource.GetStateCbor(),
			Labels:  resource.GetLabels(),
		})
	}
	return resources, nil
}

// RunAction runs one of the actions, sending each line of its output to line. It
// returns when the action is done, or with the error that ended it.
func (c *ProvisionerClient) RunAction(ctx context.Context, request ActionRequest, line func(string)) error {
	stream, err := c.rpc.RunAction(ctx, connect.NewRequest(&pluginv1.ProvisionerServiceRunActionRequest{
		Action:        request.Action,
		ArgsCbor:      request.Args,
		EntryArgsCbor: request.EntryArgs,
		Engine:        engineProto(request.Engine),
	}))
	if err != nil {
		return err
	}
	defer func() { _ = stream.Close() }()
	for stream.Receive() {
		line(stream.Msg().GetLine())
	}
	if err := stream.Err(); err != nil {
		return err
	}
	return nil
}

// Run serves a provisioner until the process is asked to stop.
//
// It is what a provisioner's main calls: it reads the socket from the
// environment the server wrote, serves ProvisionerService on it, and exits when
// SIGTERM or SIGINT arrives or the server closes the call.
func Run(provisioner Provisioner) error {
	ctx, stop := signalContext()
	defer stop()
	return ServeProvisioner(ctx, provisioner)
}

// ServeProvisioner serves provisioner on ROOKERY_PLUGIN_SOCKET until ctx is done,
// and returns after the socket is closed.
func ServeProvisioner(ctx context.Context, provisioner Provisioner) error {
	config, err := ConfigFromEnv()
	if err != nil {
		return err
	}
	return ServeProvisionerOn(ctx, config.PluginSocket, provisioner)
}

// ServeProvisionerOn serves provisioner on a Unix socket the caller names.
func ServeProvisionerOn(ctx context.Context, socket string, provisioner Provisioner) error {
	mux := http.NewServeMux()
	path, handler := pluginv1connect.NewProvisionerServiceHandler(provisionerService{provisioner: provisioner})
	mux.Handle(path, handler)
	return Serve(ctx, socket, mux)
}

// provisionerService is a Provisioner as the generated handler wants it.
type provisionerService struct {
	provisioner Provisioner
}

func (s provisionerService) Describe(ctx context.Context, _ *connect.Request[pluginv1.ProvisionerServiceDescribeRequest]) (*connect.Response[pluginv1.ProvisionerServiceDescribeResponse], error) {
	description, err := s.provisioner.Describe(ctx)
	if err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&pluginv1.ProvisionerServiceDescribeResponse{Description: descriptionProto(description)}), nil
}

func (s provisionerService) Status(ctx context.Context, request *connect.Request[pluginv1.ProvisionerServiceStatusRequest]) (*connect.Response[pluginv1.ProvisionerServiceStatusResponse], error) {
	status, err := s.provisioner.Status(ctx, engineFromProto(request.Msg.GetEngine()), request.Msg.GetArgsCbor())
	if err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&pluginv1.ProvisionerServiceStatusResponse{Up: status.Up, Message: status.Message}), nil
}

func (s provisionerService) Create(ctx context.Context, request *connect.Request[pluginv1.ProvisionerServiceCreateRequest]) (*connect.Response[pluginv1.ProvisionerServiceCreateResponse], error) {
	instance, err := s.provisioner.Create(ctx, CreateRequest{
		Engine: engineFromProto(request.Msg.GetEngine()),
		Entry:  request.Msg.GetEntry(),
		Kind:   request.Msg.GetKind(),
		Name:   request.Msg.GetName(),
		Args:   request.Msg.GetArgsCbor(),
		Seed:   request.Msg.GetSeed(),
	})
	if err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&pluginv1.ProvisionerServiceCreateResponse{
		StateCbor: instance.State,
		Address:   instance.Address,
		Id:        instance.ID,
	}), nil
}

func (s provisionerService) Get(ctx context.Context, request *connect.Request[pluginv1.ProvisionerServiceGetRequest]) (*connect.Response[pluginv1.ProvisionerServiceGetResponse], error) {
	instance, err := s.provisioner.Get(ctx, instanceFromProto(request.Msg.GetInstance()))
	if err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&pluginv1.ProvisionerServiceGetResponse{StateCbor: instance.State, Address: instance.Address}), nil
}

func (s provisionerService) Start(ctx context.Context, request *connect.Request[pluginv1.ProvisionerServiceStartRequest]) (*connect.Response[pluginv1.ProvisionerServiceStartResponse], error) {
	instance, err := s.provisioner.Start(ctx, instanceFromProto(request.Msg.GetInstance()))
	if err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&pluginv1.ProvisionerServiceStartResponse{StateCbor: instance.State, Address: instance.Address}), nil
}

func (s provisionerService) Stop(ctx context.Context, request *connect.Request[pluginv1.ProvisionerServiceStopRequest]) (*connect.Response[pluginv1.ProvisionerServiceStopResponse], error) {
	instance := instanceFromProto(request.Msg.GetInstance())
	if err := s.provisioner.Stop(ctx, instance); err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&pluginv1.ProvisionerServiceStopResponse{StateCbor: instance.State}), nil
}

func (s provisionerService) Remove(ctx context.Context, request *connect.Request[pluginv1.ProvisionerServiceRemoveRequest]) (*connect.Response[pluginv1.ProvisionerServiceRemoveResponse], error) {
	if err := s.provisioner.Remove(ctx, instanceFromProto(request.Msg.GetInstance())); err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&pluginv1.ProvisionerServiceRemoveResponse{}), nil
}

func (s provisionerService) List(ctx context.Context, request *connect.Request[pluginv1.ProvisionerServiceListRequest]) (*connect.Response[pluginv1.ProvisionerServiceListResponse], error) {
	resources, err := s.provisioner.List(ctx, ListRequest{
		Engine: engineFromProto(request.Msg.GetEngine()),
		Kind:   request.Msg.GetKind(),
		Args:   request.Msg.GetArgsCbor(),
		Entry:  request.Msg.GetEntry(),
	})
	if err != nil {
		return nil, toConnect(err)
	}
	out := make([]*pluginv1.Resource, 0, len(resources))
	for _, resource := range resources {
		out = append(out, &pluginv1.Resource{
			Kind:      resource.Kind,
			Name:      resource.Name,
			Id:        resource.ID,
			Address:   resource.Address,
			StateCbor: resource.State,
			Labels:    resource.Labels,
		})
	}
	return connect.NewResponse(&pluginv1.ProvisionerServiceListResponse{Resources: out}), nil
}

func (s provisionerService) RunAction(ctx context.Context, request *connect.Request[pluginv1.ProvisionerServiceRunActionRequest], stream *connect.ServerStream[pluginv1.ProvisionerServiceRunActionResponse]) error {
	err := s.provisioner.RunAction(ctx, ActionRequest{
		Engine:    engineFromProto(request.Msg.GetEngine()),
		Action:    request.Msg.GetAction(),
		Args:      request.Msg.GetArgsCbor(),
		EntryArgs: request.Msg.GetEntryArgsCbor(),
	}, func(line string) {
		// A failed send means the caller is gone; RunAction sees its context
		// end and returns.
		_ = stream.Send(&pluginv1.ProvisionerServiceRunActionResponse{Line: line})
	})
	if err != nil {
		return toConnect(err)
	}
	return nil
}

// toConnect maps an implementation's error to a connect error. A plugin's own
// errors are its business, so everything that is not one of the standard
// conditions is Internal.
func toConnect(err error) error {
	if err == nil {
		return nil
	}
	var connectErr *connect.Error
	if errors.As(err, &connectErr) {
		return connectErr
	}
	code := connect.CodeInternal
	switch {
	case errors.Is(err, context.Canceled):
		code = connect.CodeCanceled
	case errors.Is(err, context.DeadlineExceeded):
		code = connect.CodeDeadlineExceeded
	case errors.Is(err, ErrConfig):
		code = connect.CodeInvalidArgument
	}
	return connect.NewError(code, err)
}

func engineProto(engine Engine) *pluginv1.Engine {
	return &pluginv1.Engine{Node: engine.Node, Remote: engine.Remote}
}

func engineFromProto(engine *pluginv1.Engine) Engine {
	if engine == nil {
		return Engine{}
	}
	return Engine{Node: engine.GetNode(), Remote: engine.GetRemote()}
}

func instanceProto(instance Instance) *pluginv1.Instance {
	return &pluginv1.Instance{
		Kind:      instance.Kind,
		Name:      instance.Name,
		Id:        instance.ID,
		StateCbor: instance.State,
		Engine:    engineProto(instance.Engine),
	}
}

func instanceFromProto(instance *pluginv1.Instance) Instance {
	if instance == nil {
		return Instance{}
	}
	return Instance{
		Engine: engineFromProto(instance.GetEngine()),
		Kind:   instance.GetKind(),
		Name:   instance.GetName(),
		ID:     instance.GetId(),
		State:  instance.GetStateCbor(),
	}
}

func descriptionProto(description Description) *pluginv1.Description {
	out := &pluginv1.Description{
		Type:      description.Type,
		Label:     description.Label,
		Secrets:   description.Secrets,
		Node:      description.Node,
		Local:     description.Local,
		Kinds:     make([]*pluginv1.Kind, 0, len(description.Kinds)),
		Arguments: argumentsProto(description.Arguments),
		Sizes:     make([]*pluginv1.Size, 0, len(description.Sizes)),
		Images:    make([]*pluginv1.Image, 0, len(description.Images)),
		Actions:   make([]*pluginv1.Action, 0, len(description.Actions)),
	}
	for _, kind := range description.Kinds {
		out.Kinds = append(out.Kinds, &pluginv1.Kind{Id: kind.ID, Label: kind.Label, Machine: kind.Machine})
	}
	for _, size := range description.Sizes {
		out.Sizes = append(out.Sizes, &pluginv1.Size{Id: size.ID, Label: size.Label, Cpus: size.CPUs, MemoryMb: size.MemoryMB, DiskGb: size.DiskGB})
	}
	for _, image := range description.Images {
		out.Images = append(out.Images, &pluginv1.Image{Id: image.ID, Label: image.Label, Os: image.OS, Release: image.Release, Architecture: image.Architecture})
	}
	for _, action := range description.Actions {
		out.Actions = append(out.Actions, &pluginv1.Action{
			Name:        action.Name,
			Label:       action.Label,
			Group:       action.Group,
			Arguments:   argumentsProto(action.Arguments),
			Destructive: action.Destructive,
		})
	}
	return out
}

func descriptionFromProto(description *pluginv1.Description) Description {
	if description == nil {
		return Description{}
	}
	out := Description{
		Type:      description.GetType(),
		Label:     description.GetLabel(),
		Secrets:   description.GetSecrets(),
		Node:      description.GetNode(),
		Local:     description.GetLocal(),
		Arguments: argumentsFromProto(description.GetArguments()),
	}
	for _, kind := range description.GetKinds() {
		out.Kinds = append(out.Kinds, Kind{ID: kind.GetId(), Label: kind.GetLabel(), Machine: kind.GetMachine()})
	}
	for _, size := range description.GetSizes() {
		out.Sizes = append(out.Sizes, Size{ID: size.GetId(), Label: size.GetLabel(), CPUs: size.GetCpus(), MemoryMB: size.GetMemoryMb(), DiskGB: size.GetDiskGb()})
	}
	for _, image := range description.GetImages() {
		out.Images = append(out.Images, Image{ID: image.GetId(), Label: image.GetLabel(), OS: image.GetOs(), Release: image.GetRelease(), Architecture: image.GetArchitecture()})
	}
	for _, action := range description.GetActions() {
		out.Actions = append(out.Actions, Action{
			Name:        action.GetName(),
			Label:       action.GetLabel(),
			Group:       action.GetGroup(),
			Arguments:   argumentsFromProto(action.GetArguments()),
			Destructive: action.GetDestructive(),
		})
	}
	return out
}

func argumentsProto(arguments []Argument) []*pluginv1.Argument {
	out := make([]*pluginv1.Argument, 0, len(arguments))
	for _, argument := range arguments {
		out = append(out, &pluginv1.Argument{
			Name:        argument.Name,
			Label:       argument.Label,
			Type:        argument.Type,
			Required:    argument.Required,
			DefaultCbor: argument.Default,
			Choices:     argument.Choices,
			Help:        argument.Help,
			Secret:      argument.Secret,
		})
	}
	return out
}

func argumentsFromProto(arguments []*pluginv1.Argument) []Argument {
	out := make([]Argument, 0, len(arguments))
	for _, argument := range arguments {
		out = append(out, Argument{
			Name:     argument.GetName(),
			Label:    argument.GetLabel(),
			Type:     argument.GetType(),
			Required: argument.GetRequired(),
			Default:  argument.GetDefaultCbor(),
			Choices:  argument.GetChoices(),
			Help:     argument.GetHelp(),
			Secret:   argument.GetSecret(),
		})
	}
	return out
}

// String is a description in words, for a log line.
func (d Description) String() string {
	return fmt.Sprintf("%s (%s)", d.Type, d.Label)
}
