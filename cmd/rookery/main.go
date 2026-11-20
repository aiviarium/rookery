// Command rookery is the server and the CLI.
package main

import (
	"errors"
	"fmt"
	"os"

	"github.com/aiviarium/rookery/internal/cli"
)

// version is set by the release build with -ldflags "-X main.version=…".
var version = "dev"

func main() {
	if err := cli.Run(version); err != nil {
		fmt.Fprintln(os.Stderr, "rookery:", err)
		var exited cli.ExitStatus
		if errors.As(err, &exited) && exited > 0 && exited < 256 {
			os.Exit(int(exited))
		}
		os.Exit(1)
	}
}
