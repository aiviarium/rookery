package boxcli

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
)

// runExec starts the command locally so it inherits the user's terminal and
// standard streams. The daemon supplies the active desktop's environment.
func runExec(ctx context.Context, options Options, args []string) error {
	client, err := dial(options)
	if err != nil {
		return err
	}
	defer func() { _ = client.Close() }()
	response, err := client.Desktop.Environment(ctx, connect.NewRequest(&hostv1.DesktopServiceEnvironmentRequest{}))
	if err != nil {
		return connectError("exec", err)
	}

	environ := append([]string(nil), options.Environ...)
	for name, value := range response.Msg.GetVariables() {
		environ = append(environ, name+"="+value)
	}
	command := args
	if len(command) == 0 {
		shell := environmentValue(environ, "SHELL")
		if shell == "" {
			shell = "/bin/sh"
		}
		command = []string{shell}
	}
	path, err := commandPath(command[0], environ)
	if err != nil {
		return fmt.Errorf("box exec: %w", err)
	}
	return runCommand(ctx, append([]string{path}, command[1:]...), environ, options.Stdin, options.Stdout, options.Stderr, "")
}

// commandPath searches the desktop PATH. exec.Command would search box's own
// PATH before its child gets the supplied environment.
func commandPath(name string, environ []string) (string, error) {
	if strings.ContainsRune(name, filepath.Separator) {
		return name, nil
	}
	for _, dir := range filepath.SplitList(environmentValue(environ, "PATH")) {
		candidate := filepath.Join(dir, name)
		info, err := os.Stat(candidate)
		if err == nil && info.Mode().IsRegular() && info.Mode()&0o111 != 0 {
			if !filepath.IsAbs(candidate) {
				return "", &exec.Error{Name: name, Err: exec.ErrDot}
			}
			return candidate, nil
		}
	}
	return "", &exec.Error{Name: name, Err: exec.ErrNotFound}
}

func environmentValue(environ []string, key string) string {
	for i := len(environ) - 1; i >= 0; i-- {
		if name, value, found := strings.Cut(environ[i], "="); found && name == key {
			return value
		}
	}
	return ""
}

// runExtension runs a feature's executable from the configured box directory.
func runExtension(ctx context.Context, options Options, args []string) error {
	name := args[0]
	if name == "" || strings.ContainsAny(name, "/\\\x00") {
		return usage("box: %q is not a command name", name)
	}
	path := extensionPath(options, name)
	if _, err := os.Stat(path); err != nil {
		return usage("box: there is no command %q and no %s: run `box help`", name, path)
	}
	return runCommand(ctx, append([]string{path}, args[1:]...), options.Environ, options.Stdin, options.Stdout, options.Stderr, "")
}
