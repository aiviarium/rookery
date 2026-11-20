package cli

import (
	"fmt"
	"strconv"
	"strings"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"
	"google.golang.org/protobuf/proto"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/cborx"
)

// secretCommand is `rookery secret`: the deployment's secrets. A value comes on
// standard input, never in the command line, where `ps` would show it.
func secretCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "secret",
		Short: "List secrets' names, set one, remove one, rename one",
	}

	list := &cobra.Command{
		Use:   "list",
		Short: "List the names and scopes of the secrets",
		RunE: func(cmd *cobra.Command, _ []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			scope, _ := cmd.Flags().GetString("scope")
			listed, err := session.api.Secrets.List(cmd.Context(), connect.NewRequest(&rookeryv1.SecretServiceListRequest{Scope: scope}))
			if err != nil {
				return err
			}
			rows := make([][]string, 0, len(listed.Msg.GetSecrets()))
			messages := make([]proto.Message, 0, len(listed.Msg.GetSecrets()))
			for _, secret := range listed.Msg.GetSecrets() {
				rows = append(rows, []string{secret.GetName(), secret.GetScope()})
				messages = append(messages, secret)
			}
			return session.printer.list(messages, []string{"NAME", "SCOPE"}, rows)
		},
	}
	list.Flags().String("scope", "", "only the secrets of this scope")

	set := &cobra.Command{
		Use:   "set NAME",
		Short: "Set a secret, reading its value from standard input",
		Long: `Set a secret, reading its value from standard input.

The value is never an argument: an argument is in the process list and in a
shell's history. Pipe it, or type it when the command asks.`,
		Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			scope, _ := cmd.Flags().GetString("scope")
			value, err := readSecretLine(cmd.InOrStdin())
			if err != nil {
				return err
			}
			session, err := open(cmd)
			if err != nil {
				return err
			}
			made, err := session.api.Secrets.Set(cmd.Context(), connect.NewRequest(&rookeryv1.SecretServiceSetRequest{
				Name:  args[0],
				Scope: scope,
				Value: []byte(value),
			}))
			if err != nil {
				return err
			}
			return session.printer.line(made.Msg.GetSecret().GetName(), made.Msg.GetSecret().GetScope(), "set")
		},
	}
	set.Flags().String("scope", "global", "the scope: global, plugin/<id>, provisioner/<name> or host/<name>")

	remove := &cobra.Command{
		Use:   "remove NAME",
		Short: "Remove a secret",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			scope, _ := cmd.Flags().GetString("scope")
			if _, err := session.api.Secrets.Delete(cmd.Context(), connect.NewRequest(&rookeryv1.SecretServiceDeleteRequest{Name: args[0], Scope: scope})); err != nil {
				return err
			}
			return session.printer.line(args[0], scope, "removed")
		},
	}
	remove.Flags().String("scope", "global", "the secret's scope")

	rename := &cobra.Command{
		Use:   "rename NAME NEW_NAME",
		Short: "Give a secret another name",
		Args:  cobra.ExactArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			scope, _ := cmd.Flags().GetString("scope")
			renamed, err := session.api.Secrets.Rename(cmd.Context(), connect.NewRequest(&rookeryv1.SecretServiceRenameRequest{
				Name:    args[0],
				Scope:   scope,
				NewName: args[1],
			}))
			if err != nil {
				return err
			}
			return session.printer.line(renamed.Msg.GetSecret().GetName(), scope, "renamed")
		},
	}
	rename.Flags().String("scope", "global", "the secret's scope")

	command.AddCommand(list, set, remove, rename)
	return command
}

// settingCommand is `rookery setting`: the deployment's settings.
func settingCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "setting",
		Short: "List settings, read one, set one",
	}

	command.AddCommand(
		&cobra.Command{
			Use:   "list",
			Short: "List the settings and what they resolve to",
			RunE: func(cmd *cobra.Command, _ []string) error {
				session, err := open(cmd)
				if err != nil {
					return err
				}
				listed, err := session.api.Settings.List(cmd.Context(), connect.NewRequest(&rookeryv1.SettingServiceListRequest{}))
				if err != nil {
					return err
				}
				rows := make([][]string, 0, len(listed.Msg.GetSettings()))
				messages := make([]proto.Message, 0, len(listed.Msg.GetSettings()))
				for _, setting := range listed.Msg.GetSettings() {
					rows = append(rows, []string{setting.GetKey(), setting.GetType(), settingText(setting.GetValueCbor()), setting.GetScope()})
					messages = append(messages, setting)
				}
				return session.printer.list(messages, []string{"KEY", "TYPE", "VALUE", "FROM"}, rows)
			},
		},
		&cobra.Command{
			Use:   "get KEY",
			Short: "Read one setting",
			Args:  cobra.ExactArgs(1),
			RunE: func(cmd *cobra.Command, args []string) error {
				session, err := open(cmd)
				if err != nil {
					return err
				}
				found, err := session.api.Settings.Get(cmd.Context(), connect.NewRequest(&rookeryv1.SettingServiceGetRequest{Key: args[0]}))
				if err != nil {
					return err
				}
				return session.printer.line(settingText(found.Msg.GetSetting().GetValueCbor()))
			},
		},
		&cobra.Command{
			Use:   "set KEY VALUE",
			Short: "Set a setting; the value is read as the setting's type",
			Args:  cobra.ExactArgs(2),
			RunE: func(cmd *cobra.Command, args []string) error {
				session, err := open(cmd)
				if err != nil {
					return err
				}
				scope, _ := cmd.Flags().GetString("scope")

				current, err := session.api.Settings.Get(cmd.Context(), connect.NewRequest(&rookeryv1.SettingServiceGetRequest{Key: args[0]}))
				if err != nil {
					return err
				}
				value, err := encodeSetting(current.Msg.GetSetting().GetType(), args[1])
				if err != nil {
					return err
				}
				space, _ := cmd.Flags().GetString("space")
				set, err := session.api.Settings.Set(cmd.Context(), connect.NewRequest(&rookeryv1.SettingServiceSetRequest{
					Key:       args[0],
					Scope:     scope,
					Space:     space,
					ValueCbor: value,
				}))
				if err != nil {
					return err
				}
				return session.printer.line(set.Msg.GetSetting().GetKey(), settingText(set.Msg.GetSetting().GetValueCbor()), set.Msg.GetSetting().GetScope())
			},
		},
	)

	set, _, _ := command.Find([]string{"set"})
	if set != nil {
		set.Flags().String("scope", "global", "the scope: global, space or user")
		set.Flags().String("space", "", "the space a space-scoped value is for")
	}
	return command
}

// encodeSetting reads a command line value as a setting's type.
func encodeSetting(kind, text string) ([]byte, error) {
	switch kind {
	case "int":
		number, err := strconv.ParseInt(strings.TrimSpace(text), 10, 64)
		if err != nil {
			return nil, fmt.Errorf("cli: %q is not an integer", text)
		}
		return cborx.Marshal(number)
	case "float":
		number, err := strconv.ParseFloat(strings.TrimSpace(text), 64)
		if err != nil {
			return nil, fmt.Errorf("cli: %q is not a number", text)
		}
		return cborx.Marshal(number)
	case "bool":
		flag, err := strconv.ParseBool(strings.TrimSpace(text))
		if err != nil {
			return nil, fmt.Errorf("cli: %q is not true or false", text)
		}
		return cborx.Marshal(flag)
	default:
		return cborx.Marshal(text)
	}
}

// settingText is a setting's value in words.
func settingText(value []byte) string {
	var text string
	if err := cborx.Unmarshal(value, &text); err == nil {
		return text
	}
	var flag bool
	if err := cborx.Unmarshal(value, &flag); err == nil {
		return strconv.FormatBool(flag)
	}
	var number int64
	if err := cborx.Unmarshal(value, &number); err == nil {
		return strconv.FormatInt(number, 10)
	}
	var decimal float64
	if err := cborx.Unmarshal(value, &decimal); err == nil {
		return strconv.FormatFloat(decimal, 'g', -1, 64)
	}
	return string(value)
}

// connectionCommand is `rookery connection`: connections to outside services.
func connectionCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "connection",
		Short: "List connection types and connections, add one, test one, remove one",
	}

	types := &cobra.Command{
		Use:   "types",
		Short: "List the connection types this deployment declares",
		RunE: func(cmd *cobra.Command, _ []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			listed, err := session.api.Connections.ListTypes(cmd.Context(), connect.NewRequest(&rookeryv1.ConnectionServiceListTypesRequest{}))
			if err != nil {
				return err
			}
			rows := make([][]string, 0, len(listed.Msg.GetTypes()))
			messages := make([]proto.Message, 0, len(listed.Msg.GetTypes()))
			for _, declared := range listed.Msg.GetTypes() {
				var fields []string
				for _, field := range declared.GetFields() {
					name := field.GetName()
					if field.GetSecret() {
						name += "(secret)"
					}
					fields = append(fields, name)
				}
				rows = append(rows, []string{declared.GetId(), declared.GetLabel(), strings.Join(fields, ",")})
				messages = append(messages, declared)
			}
			return session.printer.list(messages, []string{"TYPE", "LABEL", "FIELDS"}, rows)
		},
	}

	list := &cobra.Command{
		Use:   "list",
		Short: "List connections",
		RunE: func(cmd *cobra.Command, _ []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			connectionType, _ := cmd.Flags().GetString("type")
			listed, err := session.api.Connections.List(cmd.Context(), connect.NewRequest(&rookeryv1.ConnectionServiceListRequest{Type: connectionType}))
			if err != nil {
				return err
			}
			rows := make([][]string, 0, len(listed.Msg.GetConnections()))
			messages := make([]proto.Message, 0, len(listed.Msg.GetConnections()))
			for _, connection := range listed.Msg.GetConnections() {
				rows = append(rows, []string{connection.GetId(), connection.GetType(), connection.GetName()})
				messages = append(messages, connection)
			}
			return session.printer.list(messages, []string{"ID", "TYPE", "NAME"}, rows)
		},
	}
	list.Flags().String("type", "", "only the connections of this type")

	add := &cobra.Command{
		Use:   "add TYPE NAME [FIELD=VALUE…]",
		Short: "Add a connection",
		Args:  cobra.MinimumNArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			values, err := fieldValues(args[2:])
			if err != nil {
				return err
			}
			session, err := open(cmd)
			if err != nil {
				return err
			}
			raw, err := cborx.Marshal(values)
			if err != nil {
				return err
			}
			made, err := session.api.Connections.Create(cmd.Context(), connect.NewRequest(&rookeryv1.ConnectionServiceCreateRequest{
				Type:       args[0],
				Name:       args[1],
				ValuesCbor: raw,
			}))
			if err != nil {
				return err
			}
			return session.printer.line(made.Msg.GetConnection().GetId(), made.Msg.GetConnection().GetName())
		},
	}

	remove := &cobra.Command{
		Use:   "remove NAME",
		Short: "Remove a connection",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			connection, err := session.findConnection(cmd, args[0])
			if err != nil {
				return err
			}
			if _, err := session.api.Connections.Delete(cmd.Context(), connect.NewRequest(&rookeryv1.ConnectionServiceDeleteRequest{Connection: connection.GetId()})); err != nil {
				return err
			}
			return session.printer.line(connection.GetName(), "removed")
		},
	}

	test := &cobra.Command{
		Use:   "test NAME",
		Short: "Test a connection",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			connection, err := session.findConnection(cmd, args[0])
			if err != nil {
				return err
			}
			tested, err := session.api.Connections.Test(cmd.Context(), connect.NewRequest(&rookeryv1.ConnectionServiceTestRequest{Connection: connection.GetId()}))
			if err != nil {
				return err
			}
			return session.printer.line(connection.GetName(), strconv.FormatBool(tested.Msg.GetOk()), tested.Msg.GetMessage())
		},
	}

	command.AddCommand(types, list, add, remove, test)
	return command
}

// fieldValues reads FIELD=VALUE arguments.
func fieldValues(args []string) (map[string]string, error) {
	values := make(map[string]string, len(args))
	for _, argument := range args {
		name, value, found := strings.Cut(argument, "=")
		if !found || name == "" {
			return nil, fmt.Errorf("cli: %q is not FIELD=VALUE", argument)
		}
		values[name] = value
	}
	return values, nil
}

// findConnection is the connection a name or an id names.
func (s *session) findConnection(cmd *cobra.Command, name string) (*rookeryv1.Connection, error) {
	listed, err := s.api.Connections.List(cmd.Context(), connect.NewRequest(&rookeryv1.ConnectionServiceListRequest{}))
	if err != nil {
		return nil, err
	}
	for _, connection := range listed.Msg.GetConnections() {
		if connection.GetId() == name || connection.GetName() == name {
			return connection, nil
		}
	}
	return nil, fmt.Errorf("cli: there is no connection called %s", name)
}
