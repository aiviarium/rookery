package agent_test

import (
	"context"
	"os/user"
	"testing"
	"time"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/agent"
)

// publish adds a message to a topic.
func publish(t *testing.T, client *agent.Client, topic string, keep hostv1.ChannelKeep, payload string) *hostv1.ChannelMessage {
	t.Helper()

	response, err := client.Channel.Publish(context.Background(), connect.NewRequest(&hostv1.ChannelServicePublishRequest{
		Topic: topic, Keep: keep, PayloadCbor: []byte(payload),
	}))
	if err != nil {
		t.Fatalf("publishing to %s: %v", topic, err)
	}
	return response.Msg.GetMessage()
}

// readChannel reads a topic from a sequence.
func readChannel(t *testing.T, client *agent.Client, topic string, after uint64) []*hostv1.ChannelMessage {
	t.Helper()

	response, err := client.Channel.Read(context.Background(), connect.NewRequest(&hostv1.ChannelServiceReadRequest{
		Topic: topic, After: after,
	}))
	if err != nil {
		t.Fatalf("reading %s: %v", topic, err)
	}
	return response.Msg.GetMessages()
}

// following is a client streaming a prefix.
type following struct {
	stream   *connect.ServerStreamForClient[hostv1.ChannelServiceFollowResponse]
	messages chan *hostv1.ChannelMessage
	errs     chan error
}

// follow starts a Follow stream.
func follow(t *testing.T, client *agent.Client, prefix string, since uint64) *following {
	t.Helper()

	ctx, cancel := context.WithCancel(context.Background())
	stream, err := client.Channel.Follow(ctx, connect.NewRequest(&hostv1.ChannelServiceFollowRequest{
		Prefix: prefix, Since: since,
	}))
	if err != nil {
		cancel()
		t.Fatalf("following %s: %v", prefix, err)
	}

	watched := &following{stream: stream, messages: make(chan *hostv1.ChannelMessage, 64), errs: make(chan error, 1)}
	go func() {
		for stream.Receive() {
			watched.messages <- stream.Msg().GetMessage()
		}
		watched.errs <- stream.Err()
	}()
	t.Cleanup(cancel)
	return watched
}

// next returns the next message, or fails.
func (f *following) next(t *testing.T) *hostv1.ChannelMessage {
	t.Helper()

	select {
	case message := <-f.messages:
		return message
	case err := <-f.errs:
		t.Fatalf("the follow stream ended: %v", err)
	case <-time.After(20 * time.Second):
		t.Fatal("no message arrived within 20s")
	}
	return nil
}

// quiet checks that nothing arrives for a moment.
func (f *following) quiet(t *testing.T) {
	t.Helper()

	select {
	case message := <-f.messages:
		t.Fatalf("a message arrived: %+v", message)
	case <-time.After(100 * time.Millisecond):
	}
}

func TestChannelKeepsMessages(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialRoot(t)

	t.Run("none", func(t *testing.T) {
		t.Parallel()

		topic := "example/none"
		if message := publish(t, client, topic, hostv1.ChannelKeep_CHANNEL_KEEP_NONE, "one"); message.GetSequence() == 0 {
			t.Fatal("the publish reported no sequence")
		}
		if messages := readChannel(t, client, topic, 0); len(messages) != 0 {
			t.Errorf("a topic kept as none holds %v", messages)
		}
	})

	t.Run("last", func(t *testing.T) {
		t.Parallel()

		topic := "example/last"
		publish(t, client, topic, hostv1.ChannelKeep_CHANNEL_KEEP_LAST, "one")
		last := publish(t, client, topic, hostv1.ChannelKeep_CHANNEL_KEEP_LAST, "two")

		messages := readChannel(t, client, topic, 0)
		if len(messages) != 1 || string(messages[0].GetPayloadCbor()) != "two" {
			t.Fatalf("the topic holds %v, want only two", messages)
		}
		if messages[0].GetSequence() != last.GetSequence() {
			t.Errorf("the kept message is %d, the publish said %d", messages[0].GetSequence(), last.GetSequence())
		}
	})

	t.Run("log", func(t *testing.T) {
		t.Parallel()

		topic := "example/log"
		first := publish(t, client, topic, hostv1.ChannelKeep_CHANNEL_KEEP_LOG, "one")
		publish(t, client, topic, hostv1.ChannelKeep_CHANNEL_KEEP_LOG, "two")
		publish(t, client, topic, hostv1.ChannelKeep_CHANNEL_KEEP_LOG, "three")

		messages := readChannel(t, client, topic, first.GetSequence())
		if len(messages) != 2 || string(messages[0].GetPayloadCbor()) != "two" {
			t.Fatalf("the log after %d holds %v, want two and three", first.GetSequence(), messages)
		}
	})
}

func TestChannelFollow(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialRoot(t)

	kept := publish(t, client, "example/progress", hostv1.ChannelKeep_CHANNEL_KEEP_LOG, "kept")
	watched := follow(t, client, "example/", 0)

	replayed := watched.next(t)
	if replayed.GetSequence() != kept.GetSequence() {
		t.Errorf("the replay is %d, want %d", replayed.GetSequence(), kept.GetSequence())
	}

	// A message that is not kept reaches the followers that are there.
	live := publish(t, client, "example/progress", hostv1.ChannelKeep_CHANNEL_KEEP_NONE, "live")
	if message := watched.next(t); message.GetSequence() != live.GetSequence() {
		t.Errorf("the live message is %d, want %d", message.GetSequence(), live.GetSequence())
	}

	// Another topic's is not this prefix's.
	publish(t, client, "other/progress", hostv1.ChannelKeep_CHANNEL_KEEP_NONE, "elsewhere")
	watched.quiet(t)
}

func TestChannelOnUserSocket(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialUser(t)

	current, err := user.Current()
	if err != nil {
		t.Fatalf("the test's account: %v", err)
	}

	_, err = client.Channel.Publish(context.Background(), connect.NewRequest(&hostv1.ChannelServicePublishRequest{
		Topic: "example/progress", PayloadCbor: []byte("x"),
	}))
	wantCode(t, err, connect.CodePermissionDenied)

	stream, err := client.Channel.Follow(context.Background(), connect.NewRequest(&hostv1.ChannelServiceFollowRequest{Prefix: "example/"}))
	if err == nil {
		// A server stream reports its refusal when the first message is read.
		stream.Receive()
		err = stream.Err()
	}
	wantCode(t, err, connect.CodePermissionDenied)

	topic := "user/" + current.Username + "/notes"
	publish(t, client, topic, hostv1.ChannelKeep_CHANNEL_KEEP_LOG, "mine")
	messages := readChannel(t, client, topic, 0)
	if len(messages) != 1 || string(messages[0].GetPayloadCbor()) != "mine" {
		t.Fatalf("the user's topic holds %v", messages)
	}

	watched := follow(t, client, "user/"+current.Username+"/", 0)
	if message := watched.next(t); message.GetTopic() != topic {
		t.Errorf("the follower saw %q, want %q", message.GetTopic(), topic)
	}
}

// TestChannelWithoutStore checks that a daemon with no channel store says so.
func TestChannelWithoutStore(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, func(config *agent.Config) { config.ChannelDB = "" })
	client := daemon.dialRoot(t)

	_, err := client.Channel.Publish(context.Background(), connect.NewRequest(&hostv1.ChannelServicePublishRequest{
		Topic: "example/progress", PayloadCbor: []byte("x"),
	}))
	wantCode(t, err, connect.CodeUnavailable)
}

// TestChannelTopicShape checks the error a malformed topic gives.
func TestChannelTopicShape(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialRoot(t)

	for _, topic := range []string{"example", "example/", "../etc/passwd"} {
		_, err := client.Channel.Publish(context.Background(), connect.NewRequest(&hostv1.ChannelServicePublishRequest{
			Topic: topic, PayloadCbor: []byte("x"),
		}))
		wantCode(t, err, connect.CodeInvalidArgument)
	}
}
