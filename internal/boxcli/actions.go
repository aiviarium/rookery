package boxcli

import (
	"context"
	"fmt"
	"strings"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/cborx"
)

// runActions lists the actions this host's features offer, as JSON.
func runActions(ctx context.Context, options Options, args []string) error {
	if len(args) != 0 {
		return usage("usage: box actions")
	}

	client, err := dial(options)
	if err != nil {
		return err
	}
	defer func() { _ = client.Close() }()

	response, err := client.Actions.List(ctx, connect.NewRequest(&hostv1.ActionServiceListRequest{}))
	if err != nil {
		return connectError("actions", err)
	}

	actions := make([]map[string]any, 0, len(response.Msg.GetActions()))
	for _, action := range response.Msg.GetActions() {
		arguments := make([]map[string]any, 0, len(action.GetArguments()))
		for _, argument := range action.GetArguments() {
			arguments = append(arguments, map[string]any{
				"name":     argument.GetName(),
				"label":    argument.GetLabel(),
				"required": argument.GetRequired(),
				"default":  argument.GetDefaultValue(),
			})
		}
		actions = append(actions, map[string]any{
			"name":        action.GetName(),
			"label":       action.GetLabel(),
			"group":       action.GetGroup(),
			"user":        actionUser(action.GetUser()),
			"description": action.GetDescription(),
			"terminal":    action.GetTerminal(),
			"arguments":   arguments,
		})
	}
	return writeJSON(options.Stdout, actions)
}

// actionUser is an action's account kind as the file names it.
func actionUser(user hostv1.ActionUser) string {
	switch user {
	case hostv1.ActionUser_ACTION_USER_OPERATOR:
		return "operator"
	case hostv1.ActionUser_ACTION_USER_ROOT:
		return "root"
	case hostv1.ActionUser_ACTION_USER_DESKTOP:
		return "desktop"
	default:
		return ""
	}
}

// runAction runs an action and passes what it writes through.
func runAction(ctx context.Context, options Options, args []string) error {
	if len(args) == 0 {
		return usage("usage: box run ACTION [NAME=VALUE…]")
	}
	name := args[0]

	arguments := make(map[string]any, len(args)-1)
	for _, argument := range args[1:] {
		key, value, found := strings.Cut(argument, "=")
		if !found || key == "" {
			return usage("box run: %q is not NAME=VALUE", argument)
		}
		arguments[key] = parseValue(value)
	}
	encoded, err := cborx.Marshal(arguments)
	if err != nil {
		return fmt.Errorf("box: encoding the arguments: %w", err)
	}

	client, err := dial(options)
	if err != nil {
		return err
	}
	defer func() { _ = client.Close() }()

	stream, err := client.Actions.Run(ctx, connect.NewRequest(&hostv1.ActionServiceRunRequest{
		Name: name, ArgsCbor: encoded,
	}))
	if err != nil {
		return connectError("run", err)
	}

	code := 0
	for stream.Receive() {
		switch event := stream.Msg().GetEvent().(type) {
		case *hostv1.ActionServiceRunResponse_Stdout:
			if _, err := options.Stdout.Write(event.Stdout); err != nil {
				return err
			}
		case *hostv1.ActionServiceRunResponse_Stderr:
			if _, err := options.Stderr.Write(event.Stderr); err != nil {
				return err
			}
		case *hostv1.ActionServiceRunResponse_Exit:
			code = int(event.Exit.GetCode())
		}
	}
	if err := stream.Err(); err != nil {
		return connectError("run", err)
	}
	if code != 0 {
		return &ExitError{Code: code}
	}
	return nil
}
