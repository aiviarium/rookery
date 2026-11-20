package cli

import (
	"fmt"
	"os"
	"os/signal"
	"syscall"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/server"
	"github.com/aiviarium/rookery/internal/store"
)

// serveCommand is `rookery serve`: the server.
func serveCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "serve",
		Short: "Run the server",
		Long: `Run the server.

It reads ROOKERY_DATABASE_URL and the other ROOKERY_* variables, takes the leader
lock on the database, and serves the edge and the Unix socket. A server that finds
another one leading is a standby: it answers /healthz with 503 and waits.`,
		RunE: func(cmd *cobra.Command, _ []string) error {
			config, err := server.Load()
			if err != nil {
				return err
			}
			config.Version = cmd.Root().Version
			if listen, _ := cmd.Flags().GetString("listen"); listen != "" {
				config.Listen = listen
				if config.PublicURL == "" {
					config.PublicURL = "http://" + listen
				}
			}
			if socket, _ := cmd.Flags().GetString("socket"); socket != "" {
				config.Socket = socket
			}
			if databaseURL, _ := cmd.Flags().GetString("database-url"); databaseURL != "" {
				config.DatabaseURL = databaseURL
			}

			ctx, stop := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer stop()
			return server.New(config, config.Logger()).Run(ctx)
		},
	}
	command.Flags().String("listen", "", "the edge's address, such as 127.0.0.1:4770")
	command.Flags().String("socket", "", "the Unix socket's path")
	command.Flags().String("database-url", "", "the Postgres connection string")
	return command
}

// serverCommand is `rookery server`: what the server's own machine may do.
func serverCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "server",
		Short: "Set the server up: the first admin, a token, a rekey, a reset",
	}
	command.AddCommand(
		setupKeyCommand(),
		serverTokenCommand(),
		secretsCommand(),
		resetCommand(),
		dialCommand(),
	)
	return command
}

// setupKeyCommand prints the first admin's one-time link.
func setupKeyCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "setup-key",
		Short: "Print the first admin's one-time sign-in link",
		RunE: func(cmd *cobra.Command, _ []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			made, err := session.api.Local.SetupLink(cmd.Context(), connect.NewRequest(&rookeryv1.LocalServiceSetupLinkRequest{}))
			if err != nil {
				return err
			}
			if !made.Msg.GetCreated() {
				return fmt.Errorf("cli: this deployment has an admin already: ask them for a link")
			}
			return session.printer.line(made.Msg.GetUrl())
		},
	}
}

// serverTokenCommand makes an access token for a user, from the server's machine.
func serverTokenCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "token --user EMAIL --name NAME",
		Short: "Make an access token for a user",
		RunE: func(cmd *cobra.Command, _ []string) error {
			email, _ := cmd.Flags().GetString("user")
			name, _ := cmd.Flags().GetString("name")
			if email == "" || name == "" {
				return fmt.Errorf("cli: a token needs --user and --name")
			}
			session, err := open(cmd)
			if err != nil {
				return err
			}
			made, err := session.api.Local.CreateToken(cmd.Context(), connect.NewRequest(&rookeryv1.LocalServiceCreateTokenRequest{Email: email, Name: name}))
			if err != nil {
				return err
			}
			return session.printer.line(made.Msg.GetToken())
		},
	}
	command.Flags().String("user", "", "the user the token is for, by email")
	command.Flags().String("name", "", "what the token is for, such as a script's name")
	return command
}

// secretsCommand is `rookery server secrets`: the deployment's secrets, once they
// arrive.
func secretsCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "secrets",
		Short: "The deployment's secrets",
	}
	command.AddCommand(&cobra.Command{
		Use:   "rekey",
		Short: "Reseal every secret under the current key",
		RunE: func(cmd *cobra.Command, _ []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			rekeyed, err := session.api.Local.Rekey(cmd.Context(), connect.NewRequest(&rookeryv1.LocalServiceRekeyRequest{}))
			if err != nil {
				return err
			}
			return session.printer.line(fmt.Sprintf("%d rows resealed", rekeyed.Msg.GetRows()))
		},
	})
	return command
}

// resetCommand drops and recreates the database's schemas.
func resetCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "reset",
		Short: "Drop and recreate the database's schemas",
		Long: `Drop and recreate the database's schemas.

Before the first tagged release there are no migrations: a build whose schema.sql
is not the database's refuses to start, and this is the way past it. Every row is
dropped.`,
		RunE: func(cmd *cobra.Command, _ []string) error {
			assume, err := cmd.Flags().GetBool("yes")
			if err != nil {
				return err
			}
			databaseURL := firstNonEmpty(cmd.Flag("database-url").Value.String(), os.Getenv("ROOKERY_DATABASE_URL"))
			if databaseURL == "" {
				return fmt.Errorf("cli: ROOKERY_DATABASE_URL is not set")
			}
			if !assume {
				if err := confirmReset(os.Stdin, cmd.OutOrStdout(), databaseURL); err != nil {
					return err
				}
			}

			opened, err := store.Open(cmd.Context(), store.Config{URL: databaseURL})
			if err != nil {
				return err
			}
			defer opened.Close()
			if err := opened.Reset(cmd.Context()); err != nil {
				return err
			}
			_, err = fmt.Fprintln(cmd.OutOrStdout(), "dropped and recreated the schemas of", databaseName(databaseURL))
			return err
		},
	}
	command.Flags().Bool("yes", false, "do not ask for a confirmation")
	command.Flags().String("database-url", "", "the Postgres connection string")
	return command
}
