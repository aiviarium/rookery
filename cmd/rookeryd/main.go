// Command rookeryd is the daemon on a managed host.
//
// It runs as root under systemd with two Unix sockets: one for the server, which
// reaches it by running `rookeryd connect` in an SSH session, and one for the
// host's own programs, which box uses.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"log/slog"
	"os"
	"os/signal"
	"strings"
	"syscall"

	"github.com/aiviarium/rookery/internal/agent"
)

// version is set by the release build with -ldflags "-X main.version=…".
var version = "dev"

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	if err := run(ctx, os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, "rookeryd:", err)
		os.Exit(1)
	}
}

func usage() {
	_, _ = fmt.Fprint(flag.CommandLine.Output(), `usage: rookeryd <command> [flags]

rookeryd is the daemon on a managed host. Its commands are:

  serve      answer on the root and user sockets
  connect    bridge stdin and stdout to the root socket, for the server's SSH session

flags:
`)
	flag.PrintDefaults()
}

func run(ctx context.Context, args []string) error {
	showVersion := flag.Bool("version", false, "print the version and exit")
	flag.Usage = usage
	flag.Parse()
	if *showVersion {
		fmt.Println("rookeryd", version)
		return nil
	}

	if len(args) == 0 {
		flag.Usage()
		return errors.New("no command given")
	}
	switch args[0] {
	case "serve":
		return serve(ctx, args[1:])
	case "file-worker":
		if len(args) != 2 {
			return errors.New("file-worker requires an operation")
		}
		return agent.FileWorker(ctx, args[1], os.Stdin, os.Stdout)
	case "connect":
		return connect(ctx, args[1:])
	default:
		return fmt.Errorf("unknown command %q: run `rookeryd --help`", args[0])
	}
}

// serve runs the daemon until the context is canceled.
func serve(ctx context.Context, args []string) error {
	flags := flag.NewFlagSet("rookeryd serve", flag.ContinueOnError)
	rootSocket := flags.String("root-socket", envOr("ROOKERYD_ROOT_SOCKET", agent.RootSocketPath),
		"the socket the server uses, mode 0600")
	userSocket := flags.String("user-socket", envOr("ROOKERYD_USER_SOCKET", agent.UserSocketPath),
		"the socket local programs use, mode 0666")
	channelDB := flags.String("channel-db", envOr("ROOKERYD_CHANNEL_DB", agent.ChannelDBPath),
		"the channel store")
	actions := flags.String("actions", envOr("ROOKERYD_ACTIONS", agent.ActionsPath),
		"the file of user actions the apply writes")
	desktopEnv := flags.String("desktop-env", envOr("ROOKERYD_DESKTOP_ENV", agent.DesktopEnvPath),
		"the desktop's environment file")
	operator := flags.String("operator", envOr("ROOKERYD_OPERATOR", agent.DefaultOperator),
		"the account operator actions run as")
	desktopUser := flags.String("desktop-user", envOr("ROOKERYD_DESKTOP_USER", agent.DefaultDesktopUser),
		"the account that owns the desktop")
	logLevel := flags.String("log-level", envOr("ROOKERYD_LOG_LEVEL", "info"), "debug, info, warn or error")
	if err := flags.Parse(args); err != nil {
		return err
	}
	if flags.NArg() > 0 {
		return fmt.Errorf("unexpected argument %q", flags.Arg(0))
	}

	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: parseLevel(*logLevel)})).With("program", "rookeryd")
	slog.SetDefault(logger)

	daemon, err := agent.New(ctx, agent.Config{
		Version:        version,
		RootSocket:     *rootSocket,
		UserSocket:     *userSocket,
		ChannelDB:      *channelDB,
		ActionsPath:    *actions,
		DesktopEnvPath: *desktopEnv,
		Operator:       *operator,
		DesktopUser:    *desktopUser,
		Log:            logger,
	})
	if err != nil {
		return err
	}

	logger.InfoContext(ctx, "serving", "root_socket", daemon.RootSocket(), "user_socket", daemon.UserSocket())
	return daemon.Serve(ctx)
}

// connect bridges stdin and stdout to the root socket. The server runs it in an
// SSH session through `sudo -n rookeryd connect`, and the session's stdio becomes
// its connection to the daemon.
func connect(ctx context.Context, args []string) error {
	flags := flag.NewFlagSet("rookeryd connect", flag.ContinueOnError)
	socket := flags.String("socket", envOr("ROOKERYD_ROOT_SOCKET", agent.RootSocketPath), "the root socket to bridge to")
	if err := flags.Parse(args); err != nil {
		return err
	}
	if flags.NArg() > 0 {
		return fmt.Errorf("unexpected argument %q", flags.Arg(0))
	}
	return agent.Connect(ctx, *socket, os.Stdin, os.Stdout)
}

// envOr is a ROOKERYD_* variable, or its default.
func envOr(name, fallback string) string {
	if value := os.Getenv(name); value != "" {
		return value
	}
	return fallback
}

// parseLevel turns a log level's name into one slog knows.
func parseLevel(name string) slog.Level {
	switch strings.ToLower(name) {
	case "debug":
		return slog.LevelDebug
	case "warn":
		return slog.LevelWarn
	case "error":
		return slog.LevelError
	default:
		return slog.LevelInfo
	}
}
