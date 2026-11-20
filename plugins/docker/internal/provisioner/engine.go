package provisioner

import (
	"context"
	"fmt"
	"strings"

	"github.com/moby/moby/client"

	"github.com/aiviarium/rookery/pkg/sdk"
)

// openEngine returns a Docker client for the entry's engine.
//
// A node's engine is reached through the server's link to the host: the client's
// dials go over ServerService.DialNode, so a provisioner on the server's machine
// needs no route to the node. A local entry uses this machine's socket from the
// environment, and a remote one the address in the entry's arguments.
func openEngine(_ context.Context, config sdk.Config, engine sdk.Engine, socket string) (*client.Client, error) {
	options := []client.Opt{}

	switch {
	case engine.Node != "":
		if config.Socket == "" {
			return nil, fmt.Errorf("the entry is on the node %s, and this process has no %s", engine.Node, sdk.SocketEnv)
		}
		server, err := sdk.NewClient(config)
		if err != nil {
			return nil, err
		}
		dialer := server.Node(engine.Node)
		options = append(options,
			client.WithHost("unix://"+socket),
			client.WithDialContext(dialer.DialContext),
		)
	case engine.Remote != "":
		host := engine.Remote
		if !strings.Contains(host, "://") {
			host = "tcp://" + host
		}
		options = append(options, client.WithHost(host))
	default:
		options = append(options, client.WithHostFromEnv())
	}

	opened, err := client.New(options...)
	if err != nil {
		return nil, fmt.Errorf("opening the Docker engine: %w", err)
	}
	return opened, nil
}

// machineRuntime is how a machine's container runs systemd.
type machineRuntime struct {
	// name is the container runtime, empty for the engine's default.
	name string
}

// sysbox is the runtime a machine wants: systemd in a container with the
// namespaces it needs and no privileges. The name is what the engine lists it
// as.
const sysbox = "sysbox-runc"

// chooseRuntime decides how a machine runs.
//
// sysbox is what a machine wants, and the engine feature installs it. An engine
// without it still makes machines, but unprivileged with a cgroup namespace and
// the capabilities systemd needs, never privileged: a privileged systemd reads
// the host's udev and its lid switch, and has suspended a laptop that way.
func chooseRuntime(ctx context.Context, opened *client.Client, arguments arguments) (machineRuntime, error) {
	if arguments.Runtime != "" {
		return machineRuntime{name: arguments.Runtime}, nil
	}
	info, err := opened.Info(ctx, client.InfoOptions{})
	if err != nil {
		return machineRuntime{}, fmt.Errorf("reading the engine's runtimes: %w", err)
	}
	if _, found := info.Info.Runtimes[sysbox]; found {
		return machineRuntime{name: sysbox}, nil
	}
	return machineRuntime{}, nil
}

// machineCapabilities are what systemd needs in a container that is not
// sysbox's: the mounts it sets up, its network, and the clock. Not the host's
// kernel: every other capability stays out.
func machineCapabilities() []string {
	return []string{"SYS_ADMIN", "SYS_TIME", "NET_ADMIN", "NET_RAW", "AUDIT_WRITE"}
}
