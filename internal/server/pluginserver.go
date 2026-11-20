package server

import (
	"context"
	"fmt"
	"strings"

	"connectrpc.com/connect"

	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/config"
	"github.com/aiviarium/rookery/internal/data"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/link"
	"github.com/aiviarium/rookery/internal/messaging"
	"github.com/aiviarium/rookery/internal/plugins"
	"github.com/aiviarium/rookery/internal/store"
)

// PluginServer is ServerService on the server's socket: what only a plugin's
// service may do, as the plugin it is running for.
type PluginServer struct {
	bus           *messaging.Bus
	notifications *messaging.Notifications
	topics        *messaging.Topics
	topicService  *messaging.TopicsService
	leases        *messaging.Leases
	worker        *jobs.Worker
	checker       *access.Checker
	callers       *access.CallerTokens
	installer     *plugins.Installer
	secrets       *config.Secrets
	settings      *config.Settings
	datastore     *data.Data
	store         *store.Store
	hostLinks     *link.Manager
}

// NewPluginServer returns ServerService for the events and notifications.
func NewPluginServer(bus *messaging.Bus, notifications *messaging.Notifications) *PluginServer {
	return &PluginServer{bus: bus, notifications: notifications}
}

// Publish sends an event. A plugin's events are its own namespace: the name
// starts with its id.
func (p *PluginServer) Publish(ctx context.Context, request *connect.Request[pluginv1.ServerServicePublishRequest]) (*connect.Response[pluginv1.ServerServicePublishResponse], error) {
	plugin, ok := access.PluginIdentity(ctx)
	if !ok {
		return nil, access.ToConnect(access.ErrDenied)
	}
	name := request.Msg.GetName()
	if !strings.HasPrefix(name, plugin+"/") {
		return nil, access.ToConnect(fmt.Errorf("%w: %s publishes events under its own id", access.ErrDenied, plugin))
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
	if _, ok := access.PluginIdentity(ctx); !ok {
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
