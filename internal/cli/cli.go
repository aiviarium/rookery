// Package cli is rookery's commands: the server's own, and the client the rest
// of them use.
//
// A command talks to a server: the one in ROOKERY_SERVER, or the one
// `rookery login` saved, as ROOKERY_TOKEN's token or the one it saved. On the
// server's own machine it uses the Unix socket when the address names one, and
// then it needs no token at all: the socket is the server's local admin.
package cli

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"strings"

	"github.com/spf13/cobra"
)

// Run runs the command line and returns what stopped it.
func Run(version string) error {
	root := New(version)
	root.SetArgs(append([]string{}, os.Args[1:]...))
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt)
	defer stop()
	return root.ExecuteContext(ctx)
}

// New is the command tree, for a test to drive.
func New(version string) *cobra.Command {
	var (
		server string
		asJSON bool
	)

	root := &cobra.Command{
		Use:   "rookery",
		Short: "Rookery runs a team's machines, their desktops, and the tools built on them",
		Long: `Rookery runs a team's machines, their desktops, and the tools built on them.

On the server's own machine, a command uses the server's Unix socket and needs no
sign-in. Anywhere else it reads ROOKERY_SERVER and ROOKERY_TOKEN, or the session
that ` + "`rookery login`" + ` saved.`,
		Version:       version,
		SilenceUsage:  true,
		SilenceErrors: true,
	}
	root.PersistentFlags().StringVar(&server, "server", "", "the server's address, such as https://rookery.example.test or unix:///run/rookery/rookery.sock")
	root.PersistentFlags().BoolVar(&asJSON, "json", false, "print protobuf JSON instead of a table")

	root.AddCommand(
		serveCommand(),
		serverCommand(),
		loginCommand(),
		logoutCommand(),
		whoamiCommand(),
		userCommand(),
		groupCommand(),
		spaceCommand(),
		grantCommand(),
		revokeCommand(),
		grantsCommand(),
		jobCommand(),
		rolloutCommand(),
		provisionerCommand(),
		scheduleCommand(),
		secretCommand(),
		settingCommand(),
		connectionCommand(),
		dataCommand(),
		pluginCommand(),
		registryCommand(),
		hostCommand(),
		tailnetCommand(),
		desktopCommand(),
		screenshotCommand(),
		applyCommand(),
		runActionCommand(),
		execCommand(),
		sshCommand(),
		portCommand(),
		cpCommand(),
		channelCommand(),
		hostCallCommand(),
		keyCommand(),
	)
	return root
}

// session is a command's connection and its output, made once per command.
type session struct {
	api     *api
	printer *printer
}

// open connects and returns the command's session.
func open(cmd *cobra.Command) (*session, error) {
	server, err := cmd.Flags().GetString("server")
	if err != nil {
		return nil, err
	}
	asJSON, err := cmd.Flags().GetBool("json")
	if err != nil {
		return nil, err
	}

	address, token, err := resolve(server)
	if err != nil {
		return nil, err
	}
	connection, err := dial(cmd.Context(), address, token)
	if err != nil {
		return nil, err
	}
	return &session{api: connection, printer: &printer{out: cmd.OutOrStdout(), asJSON: asJSON}}, nil
}

// resolve is the address and token a command uses: the flag, then the
// environment, then the saved session.
func resolve(flagServer string) (string, string, error) {
	saved, err := loadClientConfig()
	if err != nil {
		return "", "", err
	}

	address := firstNonEmpty(flagServer, os.Getenv("ROOKERY_SERVER"), saved.Server)
	token := firstNonEmpty(os.Getenv("ROOKERY_TOKEN"), saved.Token)

	if address == "" {
		socket := os.Getenv("ROOKERY_SOCKET")
		if socket == "" {
			socket = defaultSocket
		}
		if _, err := os.Stat(socket); err == nil { //nolint:gosec // the operator's ROOKERY_SOCKET
			address = "unix://" + socket
		}
	}
	if address == "" {
		return "", "", fmt.Errorf("cli: no server: set ROOKERY_SERVER, pass --server, or run `rookery login URL`")
	}
	return address, token, nil
}

// firstNonEmpty is the first value that is not empty.
func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if strings.TrimSpace(value) != "" {
			return value
		}
	}
	return ""
}
