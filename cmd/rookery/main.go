// Command rookery is the server and the CLI.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"os"
)

// version is set by the release build with -ldflags "-X main.version=…".
var version = "dev"

func main() {
	showVersion := flag.Bool("version", false, "print the version and exit")
	flag.Usage = usage
	flag.Parse()

	if *showVersion {
		fmt.Println("rookery", version)
		return
	}

	if err := run(context.Background(), flag.Args()); err != nil {
		fmt.Fprintln(os.Stderr, "rookery:", err)
		os.Exit(1)
	}
}

func usage() {
	_, _ = fmt.Fprint(flag.CommandLine.Output(), `usage: rookery <command> [flags]

rookery is the server and the CLI. Its commands arrive with the items that
implement them. Today it has:

  server reset    drop and recreate the database's schemas

flags:
`)
	flag.PrintDefaults()
}

// run is the CLI's one dispatch. Commands are added to it as the services they
// talk to arrive; cobra replaces this once there are enough of them.
func run(ctx context.Context, args []string) error {
	if len(args) == 0 {
		flag.Usage()
		return errors.New("no command given")
	}

	switch args[0] {
	case "serve":
		return errors.New("serve arrives with the server")
	case "server":
		return serverCommand(ctx, args[1:])
	default:
		return fmt.Errorf("unknown command %q: run `rookery --help`", args[0])
	}
}
