package cli

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"sync"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"
	"golang.org/x/term"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
)

func sshCommand() *cobra.Command {
	var account, attach string
	command := &cobra.Command{
		Use:   "ssh NAME [-- COMMAND [ARG...]]",
		Short: "Open a terminal on a host",
		Args:  cobra.MinimumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			if session.printer.asJSON {
				return fmt.Errorf("cli: an interactive terminal has no JSON output")
			}
			if attach != "" {
				if len(args) != 1 || account != "" {
					return fmt.Errorf("cli: use --attach by itself to resume a terminal")
				}
				return attachTerminal(cmd, session, attach)
			}
			tty := &hostv1.SessionServiceCreateRequest{User: &hostv1.User{Name: account}}
			if len(args) > 1 {
				tty.Command, tty.Args = args[1], args[2:]
			}
			tty.Cols, tty.Rows = terminalSize(cmd.OutOrStdout())
			opened, err := session.api.Terminals.Open(cmd.Context(), connect.NewRequest(&rookeryv1.TerminalServiceOpenRequest{
				Host: args[0], Session: tty, Private: true,
			}))
			if err != nil {
				return err
			}
			return attachTerminal(cmd, session, opened.Msg.GetTerminal().GetSession().GetId())
		},
	}
	command.Flags().StringVar(&account, "as", "", "desktop, operator or root")
	command.Flags().StringVar(&attach, "attach", "", "reattach to an existing terminal id")
	return command
}

func attachTerminal(cmd *cobra.Command, session *session, id string) error {
	ctx, cancel := context.WithCancel(cmd.Context())
	defer cancel()

	stream := session.api.Terminals.Attach(ctx)
	input := cmd.InOrStdin()
	if terminal, ok := input.(*os.File); ok {
		fd := int(terminal.Fd()) //nolint:gosec // small descriptor
		if term.IsTerminal(fd) {
			state, err := term.MakeRaw(fd)
			if err != nil {
				return fmt.Errorf("cli: setting terminal to raw mode: %w", err)
			}
			defer func() { _ = term.Restore(fd, state) }()
		}
	}

	var sendMu sync.Mutex
	send := func(message *hostv1.SessionServiceAttachRequest) error {
		sendMu.Lock()
		defer sendMu.Unlock()
		return stream.Send(&rookeryv1.TerminalServiceAttachRequest{Attach: message})
	}
	cols, rows := terminalSize(cmd.OutOrStdout())
	if err := send(&hostv1.SessionServiceAttachRequest{Id: id, Cols: cols, Rows: rows}); err != nil && !errors.Is(err, io.EOF) {
		return err
	}

	stopResize := watchTerminalSize(ctx, cmd.OutOrStdout(), func(cols, rows uint32) {
		_ = send(&hostv1.SessionServiceAttachRequest{Cols: cols, Rows: rows})
	})
	defer func() { cancel(); stopResize() }()

	inputErr := make(chan error, 1)
	go func() {
		err := streamTerminalInput(input, send)
		if !errors.Is(err, io.EOF) {
			inputErr <- err
			cancel()
		}
		sendMu.Lock()
		defer sendMu.Unlock()
		_ = stream.CloseRequest()
	}()

	for {
		message, err := stream.Receive()
		if err != nil {
			select {
			case input := <-inputErr:
				return input
			default:
			}
			if errors.Is(err, io.EOF) {
				return fmt.Errorf("cli: terminal stream ended before the process exited")
			}
			return err
		}

		event := message.GetEvent()
		if len(event.GetOutput()) > 0 {
			if _, err := cmd.OutOrStdout().Write(event.GetOutput()); err != nil {
				return err
			}
		}
		if event.GetDropped() {
			return fmt.Errorf("cli: output was dropped, reattach to terminal %s", id)
		}
		if event.GetExited() {
			if event.GetExitCode() != 0 {
				return ExitStatus(event.GetExitCode())
			}
			return nil
		}
	}
}

func streamTerminalInput(input io.Reader, send func(*hostv1.SessionServiceAttachRequest) error) error {
	buffer := make([]byte, 4096)
	for {
		n, err := input.Read(buffer)
		if n > 0 {
			if err := send(&hostv1.SessionServiceAttachRequest{Input: buffer[:n]}); err != nil {
				return err
			}
		}
		if err != nil {
			return err
		}
	}
}

func terminalSize(output io.Writer) (uint32, uint32) {
	file, ok := output.(*os.File)
	if !ok {
		return 0, 0
	}
	cols, rows, err := term.GetSize(int(file.Fd())) //nolint:gosec // small descriptor
	if err != nil || cols <= 0 || cols > 65535 || rows <= 0 || rows > 65535 {
		return 0, 0
	}
	return uint32(cols), uint32(rows) //nolint:gosec // bounded above
}
