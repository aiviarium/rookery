package boxcli

import (
	"context"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
)

// runSecret prints a host secret granted to the account running box.
func runSecret(ctx context.Context, options Options, args []string) error {
	if len(args) != 1 {
		return usage("usage: box secret NAME")
	}
	client, err := dial(options)
	if err != nil {
		return err
	}
	defer func() { _ = client.Close() }()
	response, err := client.Secrets.Get(ctx, connect.NewRequest(&hostv1.SecretServiceGetRequest{Name: args[0]}))
	if err != nil {
		return connectError("secret", err)
	}
	_, err = options.Stdout.Write(response.Msg.GetValue())
	return err
}
