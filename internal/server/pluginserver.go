package server

import (
	"context"
	"fmt"
	"strings"

	"connectrpc.com/connect"

	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/messaging"
)

// PluginServer is ServerService on the server's socket: what only a plugin's
// service may do, as the plugin it is running for.
type PluginServer struct {
	bus           *messaging.Bus
	notifications *messaging.Notifications
}

// NewPluginServer returns ServerService for the events and notifications.
func NewPluginServer(bus *messaging.Bus, notifications *messaging.Notifications) *PluginServer {
	return &PluginServer{bus: bus, notifications: notifications}
}

// DialNode arrives with the hosts' links; a plugin on the server has no path to
// a node there yet.
func (p *PluginServer) DialNode(context.Context, *connect.BidiStream[pluginv1.ServerServiceDialNodeRequest, pluginv1.ServerServiceDialNodeResponse]) error {
	return connect.NewError(connect.CodeUnimplemented, fmt.Errorf("server: DialNode arrives with the tailnet"))
}

// Publish sends an event. A plugin's events are its own namespace: the name
// starts with its id.
func (p *PluginServer) Publish(ctx context.Context, request *connect.Request[pluginv1.ServerServicePublishRequest]) (*connect.Response[pluginv1.ServerServicePublishResponse], error) {
	principal := access.MustPrincipal(ctx)
	if principal.Kind != access.KindPluginPrincipal {
		return nil, access.ToConnect(access.ErrDenied)
	}
	name := request.Msg.GetName()
	if !strings.HasPrefix(name, principal.ID+"/") {
		return nil, access.ToConnect(fmt.Errorf("%w: %s publishes events under its own id", access.ErrDenied, principal.ID))
	}
	if request.Msg.GetResource() != "" {
		if _, err := access.ParseResource(request.Msg.GetResource()); err != nil {
			return nil, access.ToConnect(err)
		}
	}
	p.bus.Publish(messaging.Event{
		Name:     name,
		Resource: request.Msg.GetResource(),
		Payload:  request.Msg.GetPayloadCbor(),
	})
	return connect.NewResponse(&pluginv1.ServerServicePublishResponse{}), nil
}

// SendNotification tells one user something.
func (p *PluginServer) SendNotification(ctx context.Context, request *connect.Request[pluginv1.ServerServiceSendNotificationRequest]) (*connect.Response[pluginv1.ServerServiceSendNotificationResponse], error) {
	principal := access.MustPrincipal(ctx)
	if principal.Kind != access.KindPluginPrincipal {
		return nil, access.ToConnect(access.ErrDenied)
	}
	sent, err := p.notifications.Send(ctx, messaging.Notification{
		UserID:   request.Msg.GetUserId(),
		Title:    request.Msg.GetTitle(),
		Body:     request.Msg.GetBody(),
		Link:     request.Msg.GetLink(),
		Resource: request.Msg.GetResource(),
		Actions:  request.Msg.GetActionsCbor(),
	})
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&pluginv1.ServerServiceSendNotificationResponse{Id: sent.ID}), nil
}
