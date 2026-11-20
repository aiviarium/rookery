// The Docker plugin's provisioner: machines and compose projects on a Docker
// engine.
//
// It is one executable for both kinds, as the manifest says. The server starts
// it for a call, with the server's socket and a token in its environment, and
// stops it after. When the entry is on a node, the engine's socket is reached
// through ServerService.DialNode; when it is local, through this machine's own
// socket; when it is remote, at the address in the entry's arguments.
package main

import (
	"fmt"
	"os"

	"github.com/aiviarium/rookery/pkg/sdk"
	"github.com/aiviarium/rookery/plugins/docker/internal/provisioner"
)

func main() {
	config, err := sdk.ConfigFromEnv()
	if err != nil {
		fmt.Fprintf(os.Stderr, "docker provisioner: %v\n", err)
		os.Exit(1)
	}
	if err := sdk.Run(provisioner.New(config)); err != nil {
		fmt.Fprintf(os.Stderr, "docker provisioner: %v\n", err)
		os.Exit(1)
	}
}
