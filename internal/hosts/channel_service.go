package hosts

import (
	"context"
	"fmt"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
)

// ChannelService publishes to a host and reads the messages it keeps.
type ChannelService struct{ *Runtime }

// Publish writes a message to the host channel.
func (s *ChannelService) Publish(ctx context.Context, request *connect.Request[rookeryv1.ChannelServicePublishRequest]) (*connect.Response[rookeryv1.ChannelServicePublishResponse], error) {
	message := request.Msg.GetPublish()
	if message == nil {
		return nil, access.ToConnect(fmt.Errorf("%w: channel message is required", access.ErrInvalid))
	}
	client, release, err := s.use(ctx, request.Msg.GetHost(), access.Use, CapabilityChannel)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	defer release()
	response, err := client.Channel.Publish(ctx, connect.NewRequest(message))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ChannelServicePublishResponse{Message: response.Msg.GetMessage()}), nil
}

// Read returns the messages the daemon retained.
func (s *ChannelService) Read(ctx context.Context, request *connect.Request[rookeryv1.ChannelServiceReadRequest]) (*connect.Response[rookeryv1.ChannelServiceReadResponse], error) {
	message := request.Msg.GetRead()
	if message == nil {
		return nil, access.ToConnect(fmt.Errorf("%w: channel topic is required", access.ErrInvalid))
	}
	client, release, err := s.use(ctx, request.Msg.GetHost(), access.Use, CapabilityChannel)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	defer release()
	response, err := client.Channel.Read(ctx, connect.NewRequest(message))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ChannelServiceReadResponse{Messages: response.Msg.GetMessages(), Next: response.Msg.GetNext()}), nil
}

// Follow streams live messages from the host, including unkept messages.
func (s *ChannelService) Follow(ctx context.Context, request *connect.Request[rookeryv1.ChannelServiceFollowRequest], stream *connect.ServerStream[rookeryv1.ChannelServiceFollowResponse]) error {
	client, release, err := s.use(ctx, request.Msg.GetHost(), access.Use, CapabilityChannel)
	if err != nil {
		return access.ToConnect(err)
	}
	defer release()
	follow := request.Msg.GetFollow()
	if follow == nil {
		follow = &hostv1.ChannelServiceFollowRequest{}
	}
	upstream, err := client.Channel.Follow(ctx, connect.NewRequest(follow))
	if err != nil {
		return access.ToConnect(err)
	}
	defer func() { _ = upstream.Close() }()
	for upstream.Receive() {
		if err := stream.Send(&rookeryv1.ChannelServiceFollowResponse{Message: upstream.Msg().GetMessage()}); err != nil {
			return err
		}
	}
	return access.ToConnect(upstream.Err())
}

// HostCallService reaches programs registered on a host daemon.
type HostCallService struct{ *Runtime }

// List returns the host daemon's registered services.
func (s *HostCallService) List(ctx context.Context, request *connect.Request[rookeryv1.HostCallServiceListRequest]) (*connect.Response[rookeryv1.HostCallServiceListResponse], error) {
	client, release, err := s.use(ctx, request.Msg.GetHost(), access.Use, CapabilityHostServices)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	defer release()
	response, err := client.HostCalls.List(ctx, connect.NewRequest(&hostv1.HostCallServiceListRequest{}))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.HostCallServiceListResponse{Services: response.Msg.GetServices()}), nil
}

// Call invokes a registered service on the host.
func (s *HostCallService) Call(ctx context.Context, request *connect.Request[rookeryv1.HostCallServiceCallRequest]) (*connect.Response[rookeryv1.HostCallServiceCallResponse], error) {
	if request.Msg.GetService() == "rookery/desktop" {
		return nil, access.ToConnect(fmt.Errorf("%w: use the desktop service", access.ErrDenied))
	}
	if request.Msg.GetService() == "" || request.Msg.GetMethod() == "" {
		return nil, access.ToConnect(fmt.Errorf("%w: host service and method are required", access.ErrInvalid))
	}
	client, release, err := s.use(ctx, request.Msg.GetHost(), access.Use, CapabilityHostServices)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	defer release()
	response, err := client.HostCalls.Call(ctx, connect.NewRequest(&hostv1.HostCallServiceCallRequest{
		Service: request.Msg.GetService(), Method: request.Msg.GetMethod(), ArgsCbor: request.Msg.GetArgsCbor(),
	}))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.HostCallServiceCallResponse{ResultCbor: response.Msg.GetResultCbor()}), nil
}
