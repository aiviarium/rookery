// Command box is the tool for users and programs on a host.
//
// It talks only to rookeryd's socket for local programs, and acts as the account
// that runs it.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"syscall"

	"github.com/aiviarium/rookery/internal/boxcli"
)

// version is set by the release build with -ldflags "-X main.version=…".
var version = "dev"

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	if err := run(ctx, os.Args[1:]); err != nil {
		var exit *boxcli.ExitError
		if errors.As(err, &exit) {
			os.Exit(exit.ExitCode())
		}
		var usage *boxcli.UsageError
		if errors.As(err, &usage) {
			fmt.Fprintln(os.Stderr, usage.Error())
			os.Exit(2)
		}
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

// run reads box's own flags, then runs the command.
func run(ctx context.Context, args []string) error {
	flags := flag.NewFlagSet("box", flag.ContinueOnError)
	showVersion := flags.Bool("version", false, "print the version and exit")
	socket := flags.String("socket", envOr("ROOKERYD_USER_SOCKET", boxcli.SocketPath), "rookeryd's socket for local programs")
	boxDir := flags.String("box-dir", envOr("ROOKERY_BOX_DIR", boxcli.BoxDir), "where a feature's box-<name> commands are")
	flags.Usage = func() { _, _ = fmt.Fprint(flags.Output(), "usage: box [flags] <command>\n\n") }
	if err := flags.Parse(args); err != nil {
		return &boxcli.UsageError{Message: err.Error()}
	}
	if *showVersion {
		fmt.Println("box", version)
		return nil
	}

	return boxcli.Run(ctx, boxcli.Options{
		Socket:  *socket,
		BoxDir:  *boxDir,
		Version: version,
		Stdin:   os.Stdin,
		Stdout:  os.Stdout,
		Stderr:  os.Stderr,
	}, flags.Args())
}

// envOr is a ROOKERY_* variable, or its default.
func envOr(name, fallback string) string {
	if value := os.Getenv(name); value != "" {
		return value
	}
	return fallback
}
