package provisioner

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"strings"

	incus "github.com/lxc/incus/v6/client"

	"github.com/aiviarium/rookery/pkg/sdk"
)

// Secret names the provisioner declares: the client certificate a remote Incus
// trusts. They are PEM contents in the server's secrets file, never paths and
// never on a command line.
const (
	// SecretCert is the client certificate's PEM.
	SecretCert = "client.cert"

	// SecretKey is the client key's PEM.
	SecretKey = "client.key"
)

// openEngine reaches the entry's Incus.
//
// A node's Incus is reached through the server's link to the host, over its Unix
// socket; a local entry over this machine's socket; a remote one over HTTPS with
// the client certificate the provisioner's secrets hold.
func openEngine(_ context.Context, config sdk.Config, engine sdk.Engine, arguments arguments) (incus.InstanceServer, error) {
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
		socket := arguments.socket()
		httpClient := &http.Client{Transport: &http.Transport{
			DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
				return dialer.DialContext(ctx, "unix", socket)
			},
		}}
		opened, err := incus.ConnectIncusHTTP(&incus.ConnectionArgs{}, httpClient)
		if err != nil {
			return nil, fmt.Errorf("reaching Incus on %s: %w", engine.Node, err)
		}
		return opened, nil
	case engine.Remote != "":
		secrets, err := sdk.ReadSecrets(config.SecretsFile)
		if err != nil {
			return nil, err
		}
		cert, key := secrets[SecretCert], secrets[SecretKey]
		if cert == "" || key == "" {
			return nil, fmt.Errorf("the entry is at %s, which needs a client certificate: set the %s and %s secrets", engine.Remote, SecretCert, SecretKey)
		}
		address := engine.Remote
		if !strings.Contains(address, "://") {
			address = "https://" + address
		}
		opened, err := incus.ConnectIncus(address, &incus.ConnectionArgs{
			TLSClientCert: cert,
			TLSClientKey:  key,
		})
		if err != nil {
			return nil, fmt.Errorf("reaching Incus at %s: %w", engine.Remote, err)
		}
		return opened, nil
	default:
		opened, err := incus.ConnectIncusUnix(arguments.socket(), nil)
		if err != nil {
			return nil, fmt.Errorf("reaching this machine's Incus: %w", err)
		}
		return opened, nil
	}
}
