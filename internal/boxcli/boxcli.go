// Package boxcli is box's commands: the tool for users and programs on a host.
//
// box talks only to rookeryd's socket for local programs. Its calls act as the
// account that runs it, which the daemon reads from the connection itself, so
// nothing here names a user.
//
// Values on the command line and on stdout are JSON for people and scripts; box
// converts them to and from the CBOR the daemon's dynamic fields hold.
package boxcli

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/aiviarium/rookery/internal/agent"
)

// Where box finds things unless the flags say otherwise.
const (
	// SocketPath is rookeryd's socket for local programs.
	SocketPath = agent.UserSocketPath
	// BoxDir is where features install their `box <name>` commands.
	BoxDir = "/usr/lib/rookery/box"
)

// Options is what a run of box is given. Its zero values are the defaults, except
// for the writers, which a caller always sets.
type Options struct {
	// Socket is rookeryd's socket for local programs.
	Socket string

	// BoxDir is where a feature's `box-<name>` commands are.
	BoxDir string

	// Version is the build's version, which `box version` prints.
	Version string

	// Environ is the environment exec and a served command run with. Empty uses
	// the process's.
	Environ []string

	Stdin  io.Reader
	Stdout io.Writer
	Stderr io.Writer

	// logf writes what a long-running command says to a person, such as a host
	// service reconnecting. Nil writes to Stderr.
	logf func(format string, args ...any)
}

// withDefaults fills in what a caller left out.
func (o Options) withDefaults() Options {
	if o.Socket == "" {
		o.Socket = SocketPath
	}
	if o.BoxDir == "" {
		o.BoxDir = BoxDir
	}
	if o.Environ == nil {
		o.Environ = os.Environ()
	}
	if o.Stdin == nil {
		o.Stdin = strings.NewReader("")
	}
	if o.Stdout == nil {
		o.Stdout = io.Discard
	}
	if o.Stderr == nil {
		o.Stderr = io.Discard
	}
	if o.logf == nil {
		o.logf = func(format string, args ...any) {
			_, _ = fmt.Fprintf(o.Stderr, format+"\n", args...)
		}
	}
	return o
}

// ExitError is a command that ran and ended with a code, which box's own exit
// code becomes.
type ExitError struct {
	// Code is the command's exit code.
	Code int
}

func (e *ExitError) Error() string { return fmt.Sprintf("the command exited %d", e.Code) }

// ExitCode is what the process should exit with.
func (e *ExitError) ExitCode() int { return e.Code }

// UsageError is a command line box does not understand.
type UsageError struct{ Message string }

func (e *UsageError) Error() string { return e.Message }

func usage(format string, args ...any) error {
	return &UsageError{Message: fmt.Sprintf(format, args...)}
}

// Run runs one of box's commands.
func Run(ctx context.Context, options Options, args []string) error {
	options = options.withDefaults()
	if len(args) == 0 {
		return usage("%s", usageText())
	}

	switch args[0] {
	case "help", "-h", "--help":
		_, err := fmt.Fprint(options.Stdout, usageText())
		return err
	case "version":
		_, err := fmt.Fprintf(options.Stdout, "box %s\n", options.Version)
		return err
	case "exec":
		return runExec(ctx, options, args[1:])
	case "actions":
		return runActions(ctx, options, args[1:])
	case "run":
		return runAction(ctx, options, args[1:])
	case "publish":
		return runPublish(ctx, options, args[1:])
	case "listen":
		return runListen(ctx, options, args[1:])
	case "services":
		return runServices(ctx, options, args[1:])
	case "call":
		return runCall(ctx, options, args[1:])
	case "serve":
		return runServe(ctx, options, args[1:])
	case "secret":
		return runSecret(ctx, options, args[1:])
	case "desktop":
		return runDesktop(ctx, options, args[1:])
	default:
		return runExtension(ctx, options, args)
	}
}

func usageText() string {
	return `usage: box <command> [flags]

box is the tool for users and programs on a host. Its commands are:

  exec [COMMAND…]                run a command with the desktop's environment
  actions                        list the actions this host's features offer
  run ACTION [NAME=VALUE…]       run one of them
  publish TOPIC [VALUE]          publish a channel message (--keep none|last|log)
  listen PREFIX                  print channel messages (--since SEQ)
  services                       list the host's host services
  call SERVICE METHOD [VALUE]    call one
  serve SERVICE -- COMMAND…      offer one, running COMMAND per call
  secret NAME                    print a secret granted to you
  desktop …                      screenshot, click, type, key, scroll, clipboard,
                                 pause, resume, state, or serve the desktop
  version                        print box's version
  <name> …                       run box-<name> from /usr/lib/rookery/box

flags:
  --socket PATH        rookeryd's socket for local programs
  --box-dir PATH       where a feature's box-<name> commands are
`
}

func readJSON(reader io.Reader) (any, error) {
	decoder := json.NewDecoder(reader)
	decoder.UseNumber()
	var value any
	if err := decoder.Decode(&value); err != nil {
		return nil, err
	}
	var extra any
	if err := decoder.Decode(&extra); !errors.Is(err, io.EOF) {
		return nil, errors.New("expected a single JSON value")
	}
	return numbersToGo(value), nil
}

// numbersToGo turns JSON's numbers into the Go types CBOR has: integers stay
// integers.
func numbersToGo(value any) any {
	switch typed := value.(type) {
	case json.Number:
		if integer, err := typed.Int64(); err == nil {
			return integer
		}
		if unsigned, err := strconv.ParseUint(typed.String(), 10, 64); err == nil {
			return unsigned
		}
		if number, err := typed.Float64(); err == nil {
			return number
		}
		return typed.String()
	case []any:
		for i, item := range typed {
			typed[i] = numbersToGo(item)
		}
		return typed
	case map[string]any:
		for key, item := range typed {
			typed[key] = numbersToGo(item)
		}
		return typed
	default:
		return value
	}
}

// jsonable turns a decoded CBOR value into one JSON can write: CBOR's maps have
// keys of any type and JSON's are strings, and times become RFC 3339.
func jsonable(value any) any {
	switch typed := value.(type) {
	case map[any]any:
		object := make(map[string]any, len(typed))
		for key, item := range typed {
			object[fmt.Sprint(key)] = jsonable(item)
		}
		return object
	case map[string]any:
		for key, item := range typed {
			typed[key] = jsonable(item)
		}
		return typed
	case []any:
		for i, item := range typed {
			typed[i] = jsonable(item)
		}
		return typed
	case time.Time:
		return typed.UTC().Format(time.RFC3339Nano)
	default:
		return value
	}
}

// writeJSON writes a value as one JSON line, which is what box prints.
func writeJSON(w io.Writer, value any) error {
	encoder := json.NewEncoder(w)
	encoder.SetEscapeHTML(false)
	return encoder.Encode(value)
}

// runCommand runs a command with the given environment and standard streams, and
// returns the error that says how it ended as an exit code.
func runCommand(ctx context.Context, command []string, environ []string, stdin io.Reader, stdout, stderr io.Writer, dir string) error {
	cmd := exec.CommandContext(ctx, command[0], command[1:]...) //nolint:gosec // box runs what it is told to.
	cmd.Env = environ
	cmd.Dir = dir
	cmd.Stdin = stdin
	cmd.Stdout = stdout
	cmd.Stderr = stderr

	err := cmd.Run()
	if err == nil {
		return nil
	}
	var exit *exec.ExitError
	if errors.As(err, &exit) {
		return &ExitError{Code: exit.ExitCode()}
	}
	return fmt.Errorf("box: running %s: %w", command[0], err)
}

// extensionPath is the file a feature's `box <name>` command runs.
func extensionPath(options Options, name string) string {
	return filepath.Join(options.BoxDir, "box-"+name)
}

// takeFlag removes a flag and its value from arguments, wherever it is: box's
// commands read better with the flag after the values they change.
func takeFlag(args []string, name string) (value string, rest []string, err error) {
	for i := 0; i < len(args); i++ {
		arg := args[i]
		if arg == name {
			if i+1 >= len(args) {
				return "", nil, usage("%s needs a value", name)
			}
			value = args[i+1]
			rest = append(rest, args[:i]...)
			rest = append(rest, args[i+2:]...)
			return value, rest, nil
		}
		if strings.HasPrefix(arg, name+"=") {
			value = strings.TrimPrefix(arg, name+"=")
			rest = append(rest, args[:i]...)
			rest = append(rest, args[i+1:]...)
			return value, rest, nil
		}
	}
	return "", args, nil
}
