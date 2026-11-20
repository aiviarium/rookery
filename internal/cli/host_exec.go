package cli

import (
	"fmt"
	"io"
	"strconv"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
)

// ExitStatus is the status of a remote command.
type ExitStatus int32

func (e ExitStatus) Error() string {
	return "remote command exited with status " + strconv.Itoa(int(e))
}

func execCommand() *cobra.Command {
	var account string
	var stdin, desktop bool
	command := &cobra.Command{
		Use:   "exec NAME [--as desktop|operator|root] -- COMMAND [ARG...]",
		Short: "Run a command on a host",
		Args:  cobra.MinimumNArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			request := &hostv1.ExecServiceRunRequest{
				User: &hostv1.User{Name: account}, Command: args[1], Args: args[2:], Desktop: desktop,
			}
			if stdin {
				request.Stdin, err = io.ReadAll(cmd.InOrStdin())
				if err != nil {
					return fmt.Errorf("cli: reading command input: %w", err)
				}
			}
			stream, err := session.api.Exec.Run(cmd.Context(), connect.NewRequest(&rookeryv1.ExecServiceRunRequest{
				Host: args[0], Exec: request,
			}))
			if err != nil {
				return err
			}
			defer func() { _ = stream.Close() }()
			for stream.Receive() {
				message := stream.Msg()
				if session.printer.asJSON {
					if err := session.printer.message(message); err != nil {
						return err
					}
				}
				exit, err := printExecEvent(cmd, message.GetEvent(), session.printer.asJSON)
				if err != nil {
					return err
				}
				if exit != nil {
					if exit.GetCode() != 0 {
						return ExitStatus(exit.GetCode())
					}
					return nil
				}
			}
			if err := stream.Err(); err != nil {
				return err
			}
			return fmt.Errorf("cli: exec ended without an exit status")
		},
	}
	command.Flags().StringVar(&account, "as", "", "desktop, operator or root")
	command.Flags().BoolVar(&stdin, "stdin", false, "send standard input to the command")
	command.Flags().BoolVar(&desktop, "desktop", false, "use the workstation's desktop environment")
	return command
}

func printExecEvent(cmd *cobra.Command, message *hostv1.ExecServiceRunResponse, json bool) (*hostv1.Exit, error) {
	switch event := message.GetEvent().(type) {
	case *hostv1.ExecServiceRunResponse_Exit:
		return event.Exit, nil
	case *hostv1.ExecServiceRunResponse_Stdout:
		if !json {
			_, err := cmd.OutOrStdout().Write(event.Stdout)
			return nil, err
		}
	case *hostv1.ExecServiceRunResponse_Stderr:
		if !json {
			_, err := cmd.ErrOrStderr().Write(event.Stderr)
			return nil, err
		}
	}
	return nil, nil
}
