package boxcli

import (
	"context"
	"fmt"
	"strconv"
	"strings"
	"time"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/cborx"
)

// runPublish publishes one message on a topic.
func runPublish(ctx context.Context, options Options, args []string) error {
	keep, rest, err := takeFlag(args, "--keep")
	if err != nil {
		return err
	}
	if keep == "" {
		keep = "last"
	}
	if len(rest) == 0 || len(rest) > 2 {
		return usage("usage: box publish TOPIC [VALUE] [--keep none|last|log]")
	}

	keepPolicy, err := parseKeep(keep)
	if err != nil {
		return err
	}

	var value any
	if len(rest) == 2 {
		value = parseValue(rest[1])
	}
	payload, err := cborx.Marshal(value)
	if err != nil {
		return fmt.Errorf("box: encoding the value: %w", err)
	}

	client, err := dial(options)
	if err != nil {
		return err
	}
	defer func() { _ = client.Close() }()

	response, err := client.Channel.Publish(ctx, connect.NewRequest(&hostv1.ChannelServicePublishRequest{
		Topic: rest[0], PayloadCbor: payload, Keep: keepPolicy,
	}))
	if err != nil {
		return connectError("publish", err)
	}
	// The sequence is the cursor a listener resumes from.
	return writeJSON(options.Stdout, response.Msg.GetMessage().GetSequence())
}

// parseKeep reads a keep policy's name.
func parseKeep(name string) (hostv1.ChannelKeep, error) {
	switch strings.ToLower(name) {
	case "none":
		return hostv1.ChannelKeep_CHANNEL_KEEP_NONE, nil
	case "last":
		return hostv1.ChannelKeep_CHANNEL_KEEP_LAST, nil
	case "log":
		return hostv1.ChannelKeep_CHANNEL_KEEP_LOG, nil
	default:
		return hostv1.ChannelKeep_CHANNEL_KEEP_UNSPECIFIED, usage("box publish: unknown --keep %q: want none, last or log", name)
	}
}

// runListen prints messages under a prefix as JSON lines, one per message.
func runListen(ctx context.Context, options Options, args []string) error {
	since, rest, err := takeFlag(args, "--since")
	if err != nil {
		return err
	}
	if len(rest) != 1 {
		return usage("usage: box listen PREFIX [--since SEQ]")
	}
	sinceSequence := uint64(0)
	if since != "" {
		sinceSequence, err = strconv.ParseUint(since, 10, 64)
		if err != nil {
			return usage("box listen: --since %q is not a number", since)
		}
	}

	client, err := dial(options)
	if err != nil {
		return err
	}
	defer func() { _ = client.Close() }()

	stream, err := client.Channel.Follow(ctx, connect.NewRequest(&hostv1.ChannelServiceFollowRequest{
		Prefix: rest[0], Since: sinceSequence,
	}))
	if err != nil {
		return connectError("listen", err)
	}

	// Messages are read in a goroutine so that a canceled call ends the command:
	// a Receive blocked on a stream that has already delivered something does not
	// always see its context end.
	messages := make(chan *hostv1.ChannelServiceFollowResponse, 16)
	receiveErr := make(chan error, 1)
	go func() {
		for stream.Receive() {
			select {
			case messages <- stream.Msg():
			case <-ctx.Done():
				return
			}
		}
		receiveErr <- stream.Err()
	}()

	for {
		var message *hostv1.ChannelMessage
		select {
		case <-ctx.Done():
			_ = stream.Close()
			return nil
		case err := <-receiveErr:
			if err != nil {
				return connectError("listen", err)
			}
			return nil
		case response := <-messages:
			message = response.GetMessage()
		}

		line := map[string]any{
			"topic":       message.GetTopic(),
			"sequence":    message.GetSequence(),
			"publishedAt": message.GetPublishedAt().AsTime().Format(time.RFC3339Nano),
		}
		var value any
		if payload := message.GetPayloadCbor(); len(payload) > 0 {
			if err := cborx.Unmarshal(payload, &value); err != nil {
				return fmt.Errorf("box: decoding the message on %s: %w", message.GetTopic(), err)
			}
		}
		line["value"] = jsonable(value)
		if err := writeJSON(options.Stdout, line); err != nil {
			return err
		}
	}
}
