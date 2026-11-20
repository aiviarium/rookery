// The Incus plugin's container provisioner: system containers on an Incus server.
//
// The server starts it for a call, with the server's socket and a token in its
// environment, and stops it after. The server is reached through the node's Incus
// socket over ServerService.DialNode, this machine's own socket, or HTTPS with the
// client certificate the provisioner's secrets hold.
package main

import (
	"fmt"
	"os"

	"github.com/aiviarium/rookery/pkg/sdk"
	"github.com/aiviarium/rookery/plugins/incus/internal/provisioner"
)

func main() {
	config, err := sdk.ConfigFromEnv()
	if err != nil {
		fmt.Fprintf(os.Stderr, "incus container provisioner: %v\n", err)
		os.Exit(1)
	}
	if err := sdk.Run(provisioner.New(config, provisioner.TypeContainer)); err != nil {
		fmt.Fprintf(os.Stderr, "incus container provisioner: %v\n", err)
		os.Exit(1)
	}
}
