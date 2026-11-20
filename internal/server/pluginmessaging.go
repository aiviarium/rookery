package server

import (
	"context"
	"fmt"
	"math"
	"strings"
	"time"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/types/known/timestamppb"

	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
)

func pluginTopic(ctx context.Context, topic string) (string, error) {
	plugin, err := requirePlugin(ctx)
	if err != nil {
		return "", err
	}
	if strings.TrimSpace(topic) != topic || !strings.HasPrefix(topic, plugin+"/") || len(topic) == len(plugin)+1 {
		return "", access.ToConnect(fmt.Errorf("%w: topic names must start with the plugin's id", access.ErrInvalid))
	}
	return topic, nil
}

// PublishTopic publishes a durable message under the plugin's id.
func (p *PluginServer) PublishTopic(ctx context.Context, request *connect.Request[pluginv1.ServerServicePublishTopicRequest]) (*connect.Response[pluginv1.ServerServicePublishTopicResponse], error) {
	topic, err := pluginTopic(ctx, request.Msg.GetTopic())
	if err != nil {
		return nil, err
	}
	id, err := p.topics.Publish(ctx, topic, request.Msg.GetPayloadCbor())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&pluginv1.ServerServicePublishTopicResponse{Id: id}), nil
}

// SubscribeTopic streams a plugin's durable topic to a consumer group.
func (p *PluginServer) SubscribeTopic(ctx context.Context, request *connect.Request[pluginv1.ServerServiceSubscribeTopicRequest], stream *connect.ServerStream[pluginv1.ServerServiceSubscribeTopicResponse]) error {
	topic, err := pluginTopic(ctx, request.Msg.GetTopic())
	if err != nil {
		return err
	}
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	deliveries, err := p.topics.Subscribe(ctx, topic, request.Msg.GetConsumer())
	if err != nil {
		return access.ToConnect(err)
	}
	if err := stream.Send(nil); err != nil {
		return err
	}
	for delivery := range deliveries {
		err := stream.Send(&pluginv1.ServerServiceSubscribeTopicResponse{Message: &rookeryv1.TopicMessage{
			Id: delivery.ID, Topic: delivery.Topic, PayloadCbor: delivery.Payload,
			Attempts:  int32(min(delivery.Attempts, math.MaxInt32)), //nolint:gosec // clamped to int32
			CreatedAt: timestamppb.New(delivery.CreatedAt),
		}})
		if err != nil {
			return err
		}
	}
	return nil
}

// AckTopic advances a consumer group's cursor.
func (p *PluginServer) AckTopic(ctx context.Context, request *connect.Request[pluginv1.ServerServiceAckTopicRequest]) (*connect.Response[pluginv1.ServerServiceAckTopicResponse], error) {
	message := request.Msg
	topic, err := pluginTopic(ctx, message.GetTopic())
	if err != nil {
		return nil, err
	}
	if strings.TrimSpace(message.GetConsumer()) == "" || message.GetId() <= 0 {
		return nil, access.ToConnect(access.ErrInvalid)
	}
	_, err = p.topicService.Ack(ctx, connect.NewRequest(&rookeryv1.TopicServiceAckRequest{
		Topic: topic, Consumer: message.GetConsumer(), Id: message.GetId(),
	}))
	if err != nil {
		return nil, err
	}
	return connect.NewResponse(&pluginv1.ServerServiceAckTopicResponse{}), nil
}

// NackTopic asks for redelivery of a durable message.
func (p *PluginServer) NackTopic(ctx context.Context, request *connect.Request[pluginv1.ServerServiceNackTopicRequest]) (*connect.Response[pluginv1.ServerServiceNackTopicResponse], error) {
	message := request.Msg
	topic, err := pluginTopic(ctx, message.GetTopic())
	if err != nil {
		return nil, err
	}
	if strings.TrimSpace(message.GetConsumer()) == "" || message.GetId() <= 0 || message.GetDelaySeconds() > 86400 {
		return nil, access.ToConnect(access.ErrInvalid)
	}
	_, err = p.topicService.Nack(ctx, connect.NewRequest(&rookeryv1.TopicServiceNackRequest{
		Topic: topic, Consumer: message.GetConsumer(), Id: message.GetId(),
		DelaySeconds: int32(message.GetDelaySeconds()), //nolint:gosec // bounded above
		Error:        message.GetError(),
	}))
	if err != nil {
		return nil, err
	}
	return connect.NewResponse(&pluginv1.ServerServiceNackTopicResponse{}), nil
}

func pluginLease(ctx context.Context, name, holder string, seconds uint32) (string, time.Duration, error) {
	plugin, err := requirePlugin(ctx)
	if err != nil {
		return "", 0, err
	}
	if strings.TrimSpace(name) == "" || strings.TrimSpace(holder) == "" || strings.ContainsAny(name+holder, "\x00") || seconds > 86400 {
		return "", 0, access.ToConnect(access.ErrInvalid)
	}
	if seconds == 0 {
		seconds = 60
	}
	return plugin + "/" + name, time.Duration(seconds) * time.Second, nil
}

// AcquireLease takes a lock in the authenticated plugin's namespace.
func (p *PluginServer) AcquireLease(ctx context.Context, request *connect.Request[pluginv1.ServerServiceAcquireLeaseRequest]) (*connect.Response[pluginv1.ServerServiceAcquireLeaseResponse], error) {
	message := request.Msg
	name, ttl, err := pluginLease(ctx, message.GetName(), message.GetHolder(), message.GetTtlSeconds())
	if err != nil {
		return nil, err
	}
	lease, err := p.leases.Acquire(ctx, name, message.GetHolder(), ttl)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&pluginv1.ServerServiceAcquireLeaseResponse{
		Fencing: lease.Fencing, ExpiresAt: timestamppb.New(lease.Expires),
	}), nil
}

// RenewLease extends a fenced lease a plugin holds.
func (p *PluginServer) RenewLease(ctx context.Context, request *connect.Request[pluginv1.ServerServiceRenewLeaseRequest]) (*connect.Response[pluginv1.ServerServiceRenewLeaseResponse], error) {
	message := request.Msg
	name, ttl, err := pluginLease(ctx, message.GetName(), message.GetHolder(), message.GetTtlSeconds())
	if err != nil {
		return nil, err
	}
	if err := p.leases.Renew(ctx, name, message.GetHolder(), message.GetFencing(), ttl); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&pluginv1.ServerServiceRenewLeaseResponse{}), nil
}

// ReleaseLease releases a fenced lease a plugin holds.
func (p *PluginServer) ReleaseLease(ctx context.Context, request *connect.Request[pluginv1.ServerServiceReleaseLeaseRequest]) (*connect.Response[pluginv1.ServerServiceReleaseLeaseResponse], error) {
	message := request.Msg
	name, _, err := pluginLease(ctx, message.GetName(), message.GetHolder(), 0)
	if err != nil {
		return nil, err
	}
	if err := p.leases.Release(ctx, name, message.GetHolder(), message.GetFencing()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&pluginv1.ServerServiceReleaseLeaseResponse{}), nil
}
