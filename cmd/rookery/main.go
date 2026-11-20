// Command rookery is the server and the CLI.
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
		_, _ = fmt.Fprintf(flag.CommandLine.Output(), "usage: rookery [--version]\n\nrookery is the server and the CLI. The commands arrive with the server.\n")
		flag.PrintDefaults()
	}
	flag.Parse()

	if *showVersion {
		fmt.Println("rookery", version)
		return
	}

	flag.Usage()
	os.Exit(2)
}
