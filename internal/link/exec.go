package link

import (
	"context"
	"errors"
	"fmt"
	"io"
	"maps"
	"slices"
	"strings"

	"golang.org/x/crypto/ssh"

	"github.com/aiviarium/rookery/internal/access"
)

// Command is an invocation on a host that has SSH but no daemon yet.
type Command struct {
	Program string
	Args    []string
	Dir     string
	Env     map[string]string
	Root    bool
	Stdin   io.Reader
	Stdout  io.Writer
	Stderr  io.Writer
}

// RunSSH runs a noninteractive command using the host's SSH account.
func (m *Manager) RunSSH(ctx context.Context, target Target, command Command) (int, error) {
	if command.Program == "" {
		return 0, fmt.Errorf("%w: exec needs a command", access.ErrInvalid)
	}
	script, err := command.script()
	if err != nil {
		return 0, err
	}
	if command.Root && target.SSHUser != "root" {
		script = "sudo -n -- /bin/sh -c " + shellWord(script)
	}
	key, err := m.ring.Active(ctx)
	if err != nil {
		return 0, err
	}
	client, err := m.dialSSH(ctx, target, ssh.PublicKeys(key.signer), m.known.Callback(ctx, target.knownAddress(), target.Name))
	if err != nil {
		return 0, err
	}
	defer func() { _ = client.Close() }()
	stop := context.AfterFunc(ctx, func() { _ = client.Close() })
	defer stop()

	sess, err := client.NewSession()
	if err != nil {
		return 0, fmt.Errorf("%w: opening SSH session on %s: %w", access.ErrUnavailable, target.Name, err)
	}
	defer func() { _ = sess.Close() }()
	sess.Stdin, sess.Stdout, sess.Stderr = command.Stdin, command.Stdout, command.Stderr
	err = sess.Run(script)
	var exit *ssh.ExitError
	switch {
	case ctx.Err() != nil:
		return 0, ctx.Err()
	case err == nil:
		return 0, nil
	case errors.As(err, &exit) && exit.ExitStatus() >= 0:
		return exit.ExitStatus(), nil
	default:
		return 0, fmt.Errorf("%w: running SSH command on %s: %w", access.ErrUnavailable, target.Name, err)
	}
}

func (c Command) script() (string, error) {
	var command strings.Builder
	if c.Dir != "" {
		command.WriteString("cd -- " + shellWord(c.Dir) + " && ")
	}
	// SSH exec takes a shell string even for an argv invocation. Only syntax
	// selected here is interpreted by the shell.
	command.WriteString("exec env --")
	for _, name := range slices.Sorted(maps.Keys(c.Env)) {
		if name == "" || strings.ContainsAny(name, "=\x00") {
			return "", fmt.Errorf("%w: invalid environment variable name", access.ErrInvalid)
		}
		command.WriteString(" " + shellWord(name+"="+c.Env[name]))
	}
	command.WriteString(" " + shellWord(c.Program))
	for _, arg := range c.Args {
		command.WriteString(" " + shellWord(arg))
	}
	if strings.ContainsRune(command.String(), '\x00') {
		return "", fmt.Errorf("%w: exec arguments cannot contain NUL", access.ErrInvalid)
	}
	return command.String(), nil
}

func shellWord(value string) string {
	return "'" + strings.ReplaceAll(value, "'", "'\"'\"'") + "'"
}
