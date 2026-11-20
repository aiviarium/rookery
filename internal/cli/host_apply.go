package cli

import (
	"fmt"
	"strings"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/cborx"
)

func applyCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "apply NAME",
		Short: "Apply the host's profile and follow its job",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			result, err := session.api.Hosts.Apply(cmd.Context(), connect.NewRequest(&rookeryv1.HostServiceApplyRequest{Name: args[0]}))
			if err != nil {
				return err
			}
			return followJob(cmd.Context(), cmd.OutOrStdout(), session, result.Msg.GetJob(), true)
		},
	}
}

func runActionCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "run NAME ACTION [ARG=VALUE...]",
		Short: "Run a declared action on a host",
		Args:  cobra.MinimumNArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			values := make(map[string]any, len(args)-2)
			for _, argument := range args[2:] {
				name, value, found := strings.Cut(argument, "=")
				if !found || name == "" {
					return fmt.Errorf("cli: action arguments use NAME=VALUE")
				}
				if _, exists := values[name]; exists {
					return fmt.Errorf("cli: action argument %s was provided twice", name)
				}
				values[name] = cborx.ParseText(value)
			}
			encoded, err := cborx.Marshal(values)
			if err != nil {
				return err
			}
			session, err := open(cmd)
			if err != nil {
				return err
			}
			result, err := session.api.Hosts.RunAction(cmd.Context(), connect.NewRequest(&rookeryv1.HostServiceRunActionRequest{
				Name: args[0], Action: args[1], ArgsCbor: encoded,
			}))
			if err != nil {
				return err
			}
			if terminal := result.Msg.GetTerminal(); terminal != nil {
				if session.printer.asJSON {
					return session.printer.message(terminal)
				}
				return attachTerminal(cmd, session, terminal.GetSession().GetId())
			}
			return followJob(cmd.Context(), cmd.OutOrStdout(), session, result.Msg.GetJob(), true)
		},
	}
}
