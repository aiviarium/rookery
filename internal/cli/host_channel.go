package cli

import (
	"encoding/json"
	"fmt"
	"strconv"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/cborx"
)

func channelCommand() *cobra.Command {
	var keep string
	var since uint64
	command := &cobra.Command{Use: "channel", Short: "Publish and listen on a host's channel"}
	publish := &cobra.Command{
		Use:   "publish NAME TOPIC [VALUE]",
		Short: "Publish to a host topic",
		Args:  cobra.RangeArgs(2, 3),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			var policy hostv1.ChannelKeep
			switch keep {
			case "none":
				policy = hostv1.ChannelKeep_CHANNEL_KEEP_NONE
			case "last":
				policy = hostv1.ChannelKeep_CHANNEL_KEEP_LAST
			case "log":
				policy = hostv1.ChannelKeep_CHANNEL_KEEP_LOG
			default:
				return fmt.Errorf("cli: --keep must be none, last or log")
			}
			payload, err := hostValue(args[2:])
			if err != nil {
				return err
			}
			response, err := session.api.Channel.Publish(cmd.Context(), connect.NewRequest(&rookeryv1.ChannelServicePublishRequest{
				Host: args[0], Publish: &hostv1.ChannelServicePublishRequest{Topic: args[1], PayloadCbor: payload, Keep: policy},
			}))
			if err != nil {
				return err
			}
			if session.printer.asJSON {
				return session.printer.message(response.Msg)
			}
			return session.printer.line(strconv.FormatUint(response.Msg.GetMessage().GetSequence(), 10))
		},
	}
	publish.Flags().StringVar(&keep, "keep", "last", "retention on the host: none, last or log")
	listen := &cobra.Command{
		Use:   "listen NAME PREFIX",
		Short: "Stream host channel messages by topic prefix",
		Args:  cobra.ExactArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			stream, err := session.api.Channel.Follow(cmd.Context(), connect.NewRequest(&rookeryv1.ChannelServiceFollowRequest{
				Host: args[0], Follow: &hostv1.ChannelServiceFollowRequest{Prefix: args[1], Since: since},
			}))
			if err != nil {
				return err
			}
			defer func() { _ = stream.Close() }()
			encoder := json.NewEncoder(cmd.OutOrStdout())
			for stream.Receive() {
				message := stream.Msg().GetMessage()
				if session.printer.asJSON {
					if err := session.printer.message(message); err != nil {
						return err
					}
					continue
				}
				value, err := decodeHostValue(message.GetPayloadCbor())
				if err != nil {
					return err
				}
				if err := encoder.Encode(map[string]any{
					"topic": message.GetTopic(), "sequence": message.GetSequence(),
					"publishedAt": message.GetPublishedAt().AsTime(), "value": value,
				}); err != nil {
					return err
				}
			}
			if cmd.Context().Err() != nil {
				return nil
			}
			return stream.Err()
		},
	}
	listen.Flags().Uint64Var(&since, "since", 0, "resume after this channel sequence")
	command.AddCommand(publish, listen)
	return command
}

func hostCallCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "call NAME SERVICE METHOD [VALUE]",
		Short: "Call a service on a host",
		Args:  cobra.RangeArgs(3, 4),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			payload, err := hostValue(args[3:])
			if err != nil {
				return err
			}
			response, err := session.api.HostCalls.Call(cmd.Context(), connect.NewRequest(&rookeryv1.HostCallServiceCallRequest{
				Host: args[0], Service: args[1], Method: args[2], ArgsCbor: payload,
			}))
			if err != nil {
				return err
			}
			if session.printer.asJSON {
				return session.printer.message(response.Msg)
			}
			value, err := decodeHostValue(response.Msg.GetResultCbor())
			if err != nil {
				return err
			}
			return json.NewEncoder(cmd.OutOrStdout()).Encode(value)
		},
	}
}

func hostValue(argument []string) ([]byte, error) {
	if len(argument) == 0 {
		return cborx.Marshal(nil)
	}
	return cborx.Marshal(cborx.ParseText(argument[0]))
}

func decodeHostValue(payload []byte) (any, error) {
	var value any
	if len(payload) > 0 {
		if err := cborx.Unmarshal(payload, &value); err != nil {
			return nil, err
		}
	}
	return cborx.JSONValue(value), nil
}
