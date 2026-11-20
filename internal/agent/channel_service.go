package agent

import (
	"context"
	"encoding/binary"
	"fmt"
	"strings"
	"time"

	"connectrpc.com/connect"
	"go.etcd.io/bbolt"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
)

// channelService serves ChannelService: the host's topics.
type channelService struct {
	server *Server
}

func (h *channelService) Publish(ctx context.Context, request *connect.Request[hostv1.ChannelServicePublishRequest]) (*connect.Response[hostv1.ChannelServicePublishResponse], error) {
	caller, err := callerOf(ctx)
	if err != nil {
		return nil, err
	}
	if err := allowTopic(caller, request.Msg.GetTopic()); err != nil {
		return nil, err
	}
	if h.server.channels == nil {
		return nil, toConnect(fmt.Errorf("%w: the daemon has no channel store", ErrUnavailable))
	}

	message, err := h.server.channels.publish(request.Msg.GetTopic(), request.Msg.GetKeep(), request.Msg.GetPayloadCbor(), time.Now())
	if err != nil {
		return nil, toConnect(err)
	}
	h.server.log.DebugContext(ctx, "published a channel message",
		"topic", message.GetTopic(), "sequence", message.GetSequence(), "bytes", len(message.GetPayloadCbor()))
	return connect.NewResponse(&hostv1.ChannelServicePublishResponse{Message: message}), nil
}

func (h *channelService) Read(ctx context.Context, request *connect.Request[hostv1.ChannelServiceReadRequest]) (*connect.Response[hostv1.ChannelServiceReadResponse], error) {
	caller, err := callerOf(ctx)
	if err != nil {
		return nil, err
	}
	if err := allowTopic(caller, request.Msg.GetTopic()); err != nil {
		return nil, err
	}
	if h.server.channels == nil {
		return nil, toConnect(fmt.Errorf("%w: the daemon has no channel store", ErrUnavailable))
	}

	messages, err := h.server.channels.read(request.Msg.GetTopic(), request.Msg.GetAfter(), request.Msg.GetLimit())
	if err != nil {
		return nil, toConnect(err)
	}
	next := request.Msg.GetAfter()
	if len(messages) > 0 {
		next = messages[len(messages)-1].GetSequence()
	}
	return connect.NewResponse(&hostv1.ChannelServiceReadResponse{Messages: messages, Next: next}), nil
}

func (h *channelService) Follow(ctx context.Context, request *connect.Request[hostv1.ChannelServiceFollowRequest], stream *connect.ServerStream[hostv1.ChannelServiceFollowResponse]) error {
	caller, err := callerOf(ctx)
	if err != nil {
		return err
	}
	prefix := request.Msg.GetPrefix()
	if err := allowPrefix(caller, prefix); err != nil {
		return err
	}
	if h.server.channels == nil {
		return toConnect(fmt.Errorf("%w: the daemon has no channel store", ErrUnavailable))
	}

	follower, upTo, err := h.server.channels.follow(prefix)
	if err != nil {
		return toConnect(err)
	}
	defer h.server.channels.unfollow(follower)

	// The store's kept messages between the caller's sequence and the one the
	// follower registered at come first, then whatever arrives after it. Because
	// the follower exists before the store is read, nothing falls between them.
	messages, err := h.server.channels.readPrefix(prefix, request.Msg.GetSince(), upTo)
	if err != nil {
		return toConnect(err)
	}
	for _, message := range messages {
		if err := stream.Send(&hostv1.ChannelServiceFollowResponse{Message: message}); err != nil {
			return err
		}
	}

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case message, open := <-follower.messages:
			if !open {
				// The daemon is closing the channel.
				return nil
			}
			if message.GetSequence() <= upTo {
				continue
			}
			if err := stream.Send(&hostv1.ChannelServiceFollowResponse{Message: message}); err != nil {
				return err
			}
		}
	}
}

// allowTopic checks that a caller may use a topic.
func allowTopic(caller *caller, topic string) error {
	if err := validateTopic(topic); err != nil {
		return toConnect(err)
	}
	if caller.role != roleUser {
		return nil
	}
	if !strings.HasPrefix(topic, "user/"+caller.account.Name+"/") {
		return denied("agent: a local program uses topics under user/%s/, and %s is not one",
			caller.account.Name, topic)
	}
	return nil
}

// allowPrefix checks that a caller may follow topics under a prefix.
//
// A prefix is a topic or the start of one, so "example/" and "example" both mean
// the topics under example, and the empty prefix means every topic.
func allowPrefix(caller *caller, prefix string) error {
	if err := validatePrefix(prefix); err != nil {
		return toConnect(err)
	}
	if caller.role != roleUser {
		return nil
	}

	base := "user/" + caller.account.Name
	if prefix != base && !strings.HasPrefix(prefix, base+"/") {
		return denied("agent: a local program follows topics under user/%s/, and %q is not one",
			caller.account.Name, prefix)
	}
	return nil
}

// readPrefix returns the kept messages under a prefix between two sequences,
// oldest first.
func (s *channelStore) readPrefix(prefix string, after, upTo uint64) ([]*hostv1.ChannelMessage, error) {
	if upTo <= after {
		return nil, nil
	}

	var messages []*hostv1.ChannelMessage
	err := s.db.View(func(tx *bbolt.Tx) error {
		stored := tx.Bucket(messagesBucket)
		cursor := stored.Cursor()
		for key, _ := cursor.Seek(encodeSequence(after + 1)); key != nil; key, _ = cursor.Next() {
			sequence := binary.BigEndian.Uint64(key)
			if sequence > upTo {
				break
			}
			message, err := decodeMessage(stored, key)
			if err != nil {
				return err
			}
			if hasTopicPrefix(message.GetTopic(), prefix) {
				messages = append(messages, message)
			}
		}
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("agent: reading %s: %w", prefix, err)
	}
	return messages, nil
}
