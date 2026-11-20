package boxcli

import (
	"context"
	"fmt"
	"os"
	"strings"
)

// runExec runs a command with the desktop's environment: its display,
// X authority, session bus and PATH. A host with no desktop runs it with the
// environment box has.
func runExec(ctx context.Context, options Options, args []string) error {
	command := args
	if len(command) == 0 {
		command = []string{shellOf(options.Environ)}
	}

	environ := append([]string(nil), options.Environ...)
	desktop, err := desktopEnvironment(options.DesktopEnvPath)
	if err != nil {
		return err
	}
	for name, value := range desktop {
		environ = append(environ, name+"="+value)
	}

	return runCommand(ctx, command, environ, options.Stdin, options.Stdout, options.Stderr, "")
}

// desktopEnvironment reads the desktop's environment file. A host without a
// desktop has none, which is not an error: `box exec` still runs something.
func desktopEnvironment(path string) (map[string]string, error) {
	raw, err := os.ReadFile(path) //nolint:gosec // the daemon's own configuration file.
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("box: reading %s: %w", path, err)
	}

	env := make(map[string]string)
	for _, line := range strings.Split(string(raw), "\n") {
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		name, value, found := strings.Cut(line, "=")
		if !found {
			continue
		}
		value = strings.TrimSpace(value)
		if len(value) >= 2 {
			if (value[0] == '"' && value[len(value)-1] == '"') || (value[0] == '\'' && value[len(value)-1] == '\'') {
				value = value[1 : len(value)-1]
			}
		}
		if name = strings.TrimSpace(name); name != "" {
			env[name] = value
		}
	}
	return env, nil
}

// shellOf is the account's shell from an environment, for `box exec` with no
// command.
func shellOf(environ []string) string {
	for _, entry := range environ {
		if name, value, found := strings.Cut(entry, "="); found && name == "SHELL" && value != "" {
			return value
		}
	}
	return "/bin/sh"
}

// runExtension runs a feature's `box <name>` command, which is installed as
// box-<name> in the box directory.
func runExtension(ctx context.Context, options Options, args []string) error {
	path := extensionPath(options, args[0])
	if _, err := os.Stat(path); err != nil {
		return usage("box: there is no command %q and no %s: run `box help`", args[0], path)
	}
	return runCommand(ctx, append([]string{path}, args[1:]...), options.Environ, options.Stdin, options.Stdout, options.Stderr, "")
}
