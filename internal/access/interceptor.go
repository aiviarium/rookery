package access

import (
	"context"
	"fmt"
	"strings"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/types/descriptorpb"
	"google.golang.org/protobuf/types/dynamicpb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
)

// Interceptor refuses a call before its handler runs, from the access option the
// procedure declares.
//
// The option names a request field: `option (rookery.v1.access) = { level:
// LEVEL_MANAGE, resource: "space" };`. The field's value becomes a resource URI
// in one of three ways:
//
//   - it holds a URI already, such as space://01h…;
//   - the field is named for a kind, such as "space" or "host", and the value is
//     that kind's id;
//   - otherwise the service names the kind, in serviceKinds, and the value is an
//     id of it.
//
// An option with no resource checks the level against the caller alone.
//
// A bidirectional stream cannot be refused from its first message: the client
// sends it after the handler starts. Such a procedure declares a level with no
// resource, which authenticates the caller, and its handler checks the resource
// of each message itself.
type Interceptor struct {
	checker *Checker
}

// NewInterceptor returns the interceptor for a checker.
func NewInterceptor(checker *Checker) *Interceptor {
	return &Interceptor{checker: checker}
}

// WrapUnary refuses a unary call the caller may not make.
func (i *Interceptor) WrapUnary(next connect.UnaryFunc) connect.UnaryFunc {
	return func(ctx context.Context, request connect.AnyRequest) (connect.AnyResponse, error) {
		ctx, err := i.authorize(ctx, request.Spec(), request.Any())
		if err != nil {
			return nil, err
		}
		return next(ctx, request)
	}
}

// WrapStreamingHandler refuses a streaming call the caller may not make, before
// the handler reads from or writes to the stream.
func (i *Interceptor) WrapStreamingHandler(next connect.StreamingHandlerFunc) connect.StreamingHandlerFunc {
	return func(ctx context.Context, conn connect.StreamingHandlerConn) error {
		if conn.Spec().StreamType == connect.StreamTypeBidi {
			authorized, err := i.authorizeCaller(ctx, conn.Spec())
			if err != nil {
				return err
			}
			return next(authorized, conn)
		}

		// A server or client stream carries its first request message before the
		// handler runs. Read it, check it, and hand it back to the handler.
		first, err := receiveFirst(conn)
		if err != nil {
			return err
		}
		authorized, err := i.authorize(ctx, conn.Spec(), first)
		if err != nil {
			return err
		}
		return next(authorized, &replayConn{StreamingHandlerConn: conn, first: first})
	}
}

// WrapStreamingClient passes a call through: this side is a client.
func (i *Interceptor) WrapStreamingClient(next connect.StreamingClientFunc) connect.StreamingClientFunc {
	return next
}

// authorize resolves the procedure's access option and checks the caller on the
// resource the call names.
func (i *Interceptor) authorize(ctx context.Context, spec connect.Spec, request any) (context.Context, error) {
	callerCtx, err := i.authorizeCaller(ctx, spec)
	if err != nil {
		return ctx, err
	}

	method, err := MethodOf(spec)
	if err != nil {
		return ctx, err
	}
	_, field, err := MethodOption(method)
	if err != nil {
		return ctx, err
	}

	resource, err := ResourceOf(method, request, field)
	if err != nil {
		return ctx, err
	}
	callerCtx = WithResource(callerCtx, resource)

	level, _, err := MethodOption(method)
	if err != nil {
		return ctx, err
	}
	if err := i.checker.Check(callerCtx, MustPrincipal(callerCtx), resource, level); err != nil {
		return ctx, ToConnect(err)
	}
	return callerCtx, nil
}

// authorizeCaller checks the parts of an access option that are about the caller
// alone.
func (i *Interceptor) authorizeCaller(ctx context.Context, spec connect.Spec) (context.Context, error) {
	method, err := MethodOf(spec)
	if err != nil {
		return ctx, err
	}
	level, _, err := MethodOption(method)
	if err != nil {
		return ctx, err
	}
	principal, _ := PrincipalFrom(ctx)
	if err := i.checker.checkPrincipal(principal, level); err != nil {
		return ctx, ToConnect(err)
	}
	return ctx, nil
}

// MethodOption reads a procedure's access declaration. A method without one is
// an error: every procedure declares its access, and a test walks them all.
func MethodOption(method protoreflect.MethodDescriptor) (Level, string, error) {
	options, ok := method.Options().(*descriptorpb.MethodOptions)
	if !ok || options == nil {
		return 0, "", malformed("%s declares no access option", method.FullName())
	}
	if !proto.HasExtension(options, rookeryv1.E_Access) {
		return 0, "", malformed("%s declares no access option", method.FullName())
	}
	extension, ok := proto.GetExtension(options, rookeryv1.E_Access).(*rookeryv1.Access)
	if !ok || extension == nil {
		return 0, "", malformed("%s's access option is not a rookery.v1.Access", method.FullName())
	}
	if extension.GetLevel() == rookeryv1.Level_LEVEL_UNSPECIFIED {
		return 0, "", malformed("%s declares no level in its access option", method.FullName())
	}
	return extension.GetLevel(), extension.GetResource(), nil
}

// ResourceOf is the resource a call is about: the named field's value as a URI,
// or the empty string when the procedure names no resource or the field is
// empty (a create has no id yet).
func ResourceOf(method protoreflect.MethodDescriptor, request any, field string) (string, error) {
	if field == "" {
		return "", nil
	}
	message, ok := request.(proto.Message)
	if !ok || message == nil {
		return "", malformed("%s names the resource field %q but has no request message", method.FullName(), field)
	}

	value, err := fieldValue(message.ProtoReflect(), field)
	if err != nil {
		return "", malformed("%s: %v", method.FullName(), err)
	}
	if value == "" {
		return "", nil
	}
	if strings.Contains(value, "://") {
		if _, err := ParseResource(value); err != nil {
			return "", ToConnect(err)
		}
		return value, nil
	}
	if kindFields[field] {
		return field + "://" + value, nil
	}
	if kind, ok := serviceKinds[method.Parent().FullName()]; ok {
		return kind + "://" + value, nil
	}
	return "", malformed(
		"%s names the resource field %q, which is not a kind and whose service declares no kind", method.FullName(), field,
	)
}

// fieldValue reads a string field, or the id of a message field, by name.
func fieldValue(message protoreflect.Message, field string) (string, error) {
	descriptor := message.Descriptor().Fields().ByName(protoreflect.Name(field))
	switch {
	case descriptor == nil:
		return "", fmt.Errorf("the request has no field %q", field)
	case descriptor.Kind() == protoreflect.StringKind:
		return message.Get(descriptor).String(), nil
	case descriptor.Kind() == protoreflect.MessageKind:
		nested := message.Get(descriptor).Message()
		id := nested.Descriptor().Fields().ByName("id")
		if id == nil || id.Kind() != protoreflect.StringKind {
			return "", fmt.Errorf("the field %q is a message without an id", field)
		}
		return nested.Get(id).String(), nil
	default:
		return "", fmt.Errorf("the field %q is neither a string nor a message", field)
	}
}

// serviceKinds is the resource kind of the services whose request fields are
// named "id" rather than for their kind. A service that names its field for its
// kind, such as SpaceService's "space", needs no entry.
var serviceKinds = map[protoreflect.FullName]string{
	"rookery.v1.HostService":        KindHost,
	"rookery.v1.DocumentService":    KindDocument,
	"rookery.v1.RolloutService":     KindRollout,
	"rookery.v1.PluginService":      KindPlugin,
	"rookery.v1.ProvisionerService": KindProvisioner,
	"rookery.v1.JobService":         KindJob,
}

// MethodOf is the procedure's descriptor, which connect carries in its spec.
func MethodOf(spec connect.Spec) (protoreflect.MethodDescriptor, error) {
	switch schema := spec.Schema.(type) {
	case protoreflect.MethodDescriptor:
		return schema, nil
	case nil:
		return nil, malformed("%s carries no descriptor", spec.Procedure)
	default:
		return nil, malformed("%s carries a %T, not a method descriptor", spec.Procedure, schema)
	}
}

// receiveFirst reads a stream's first request message as a dynamic message.
func receiveFirst(conn connect.StreamingHandlerConn) (proto.Message, error) {
	method, err := MethodOf(conn.Spec())
	if err != nil {
		return nil, err
	}
	first := dynamicpb.NewMessage(method.Input())
	if err := conn.Receive(first); err != nil {
		return nil, connect.NewError(connect.CodeInvalidArgument, fmt.Errorf("access: reading the request: %w", err))
	}
	return first, nil
}

// replayConn hands the handler the request message the interceptor read.
type replayConn struct {
	connect.StreamingHandlerConn
	first proto.Message
}

// Receive returns the first message once, and then reads from the stream.
func (c *replayConn) Receive(message any) error {
	if c.first == nil {
		return c.StreamingHandlerConn.Receive(message)
	}
	target, ok := message.(proto.Message)
	if !ok {
		return c.StreamingHandlerConn.Receive(message)
	}
	proto.Reset(target)
	proto.Merge(target, c.first)
	c.first = nil
	return nil
}

// Methods is every procedure of the files given, by descriptor. The test that
// every registered procedure declares an access option walks it.
func Methods(files ...protoreflect.FileDescriptor) []protoreflect.MethodDescriptor {
	var methods []protoreflect.MethodDescriptor
	for _, file := range files {
		services := file.Services()
		for i := 0; i < services.Len(); i++ {
			service := services.Get(i)
			for j := 0; j < service.Methods().Len(); j++ {
				methods = append(methods, service.Methods().Get(j))
			}
		}
	}
	return methods
}
