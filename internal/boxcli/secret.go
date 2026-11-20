package boxcli

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// runSecret prints a host secret granted to the account running box.
//
// The apply writes each `host/<name>` secret as a file under the secrets
// directory, owned by the account it is for and unreadable by anyone else, so the
// kernel is what decides whether box may print it. The daemon never holds one.
func runSecret(_ context.Context, options Options, args []string) error {
	if len(args) != 1 {
		return usage("usage: box secret NAME")
	}
	name := args[0]
	if strings.ContainsAny(name, "/\x00") || name == "." || name == ".." {
		return usage("box secret: %q is not a secret's name", name)
	}

	secret, err := os.ReadFile(filepath.Join(options.SecretsDir, name)) //nolint:gosec // the name is checked above.
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return fmt.Errorf("box: there is no secret %s for %s", name, "this account")
		}
		if errors.Is(err, os.ErrPermission) {
			return fmt.Errorf("box: the secret %s is not granted to this account: %w", name, err)
		}
		return fmt.Errorf("box: reading the secret %s: %w", name, err)
	}
	_, err = options.Stdout.Write(secret)
	return err
}
