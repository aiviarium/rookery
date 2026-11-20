// Command rookeryd is the daemon on a managed host.
package main

import (
	"flag"
	"fmt"
	"os"
)

// version is set by the release build with -ldflags "-X main.version=…".
var version = "dev"

func main() {
	showVersion := flag.Bool("version", false, "print the version and exit")
	flag.Usage = func() {
		_, _ = fmt.Fprintf(flag.CommandLine.Output(), "usage: rookeryd [--version]\n\nrookeryd is the daemon on a managed host. Its commands arrive with the agent.\n")
		flag.PrintDefaults()
	}
	flag.Parse()

	if *showVersion {
		fmt.Println("rookeryd", version)
		return
	}

	flag.Usage()
	os.Exit(2)
}
