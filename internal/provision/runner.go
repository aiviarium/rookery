package provision

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/aiviarium/rookery/pkg/sdk"
)

// Runner starts a provisioner's executable for one call.
//
// Nothing is kept between two calls: the server starts the program, gives it a
// socket of its own on ROOKERY_PLUGIN_SOCKET, the server's socket on
// ROOKERY_SOCKET and a token, calls ProvisionerService, and stops it again.
// Arguments and state are CBOR the provisioner writes; the core never reads them.
type Runner struct {
	// socket is the server's Unix socket, which the provisioner is given as
	// ROOKERY_SOCKET.
	socket string

	// dir is where a call's own directory, its socket and its secrets file are
	// made. It is under the server's state directory, on disk and short.
	dir string

	// logger says when a provisioner is started and how it ended.
	logger *slog.Logger

	// timeout bounds a call that names none of its own.
	timeout time.Duration
}

// NewRunner returns a runner for the server's socket, making each call's
// directory under dir. A nil logger discards.
func NewRunner(socket, dir string, logger *slog.Logger) *Runner {
	if logger == nil {
		logger = slog.New(slog.DiscardHandler)
	}
	return &Runner{socket: socket, dir: dir, logger: logger, timeout: 30 * time.Minute}
}

// Call is one call to a provisioner.
type Call struct {
	// Entry is the entry the call is for.
	Entry Entry

	// Type is the entry's type, whose Command is the program to run.
	Type Type

	// Engine is where the entry's engine is, which the provisioner reaches with
	// DialNode or itself.
	Engine sdk.Engine

	// Token is the credential the process presents on the server's socket.
	Token string

	// Secrets are the values of the secrets the provisioner declared.
	Secrets map[string]string

	// Timeout bounds the call. Zero is the runner's.
	Timeout time.Duration

	// Line is where the process's output goes, a line at a time: the job's log,
	// or a test's. Nil discards it.
	Line func(string)
}

// Do starts the call's provisioner, hands fn its client, and stops it afterwards.
// A failure to start, a call that fails and a process that does not stop are all
// errors; fn's error is returned as it is.
func (r *Runner) Do(ctx context.Context, call Call, fn func(context.Context, *sdk.ProvisionerClient) error) error {
	if call.Type.Command == "" {
		return fmt.Errorf("%w: %s carries no executable", ErrNoType, call.Type.ID)
	}
	timeout := call.Timeout
	if timeout <= 0 {
		timeout = r.timeout
	}
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	dir, err := os.MkdirTemp(r.dir, "provisioner-")
	if err != nil {
		return fmt.Errorf("provision: making the call's directory: %w", err)
	}
	defer func() { _ = os.RemoveAll(dir) }()

	socket := filepath.Join(dir, "plugin.sock")
	environment := []string{
		"PATH=" + os.Getenv("PATH"),
		"HOME=" + os.Getenv("HOME"),
		"LANG=C.UTF-8",
		"TZ=UTC",
		sdk.SocketEnv + "=" + r.socket,
		sdk.PluginSocketEnv + "=" + socket,
	}
	if call.Token != "" {
		environment = append(environment, sdk.TokenEnv+"="+call.Token)
	}
	if len(call.Secrets) > 0 {
		secretsFile, err := writeSecrets(dir, call.Secrets)
		if err != nil {
			return err
		}
		environment = append(environment, sdk.SecretsEnv+"="+secretsFile)
	}

	//nolint:gosec // the executable is the installed plugin's, and the arguments are none.
	command := exec.CommandContext(ctx, call.Type.Command)
	command.Env = environment
	command.Dir = dir
	output := &lineWriter{line: call.Line}
	command.Stdout = output
	command.Stderr = output

	r.logger.Debug("starting a provisioner", slog.String("type", call.Type.ID), slog.String("entry", call.Entry.Name))
	started := time.Now()
	if err := command.Start(); err != nil {
		return fmt.Errorf("provision: starting %s: %w", call.Type.Command, err)
	}
	exited := make(chan error, 1)
	go func() { exited <- command.Wait() }()

	defer func() {
		if err := stop(command, exited); err != nil {
			r.logger.Warn("a provisioner did not stop cleanly", slog.String("type", call.Type.ID), slog.String("error", err.Error()))
		}
	}()

	if err := waitForSocket(ctx, socket, exited); err != nil {
		return fmt.Errorf("provision: %s did not serve on its socket: %w", call.Type.ID, err)
	}
	httpClient, base := sdk.DialSocket(socket)
	client := sdk.NewProvisionerClient(httpClient, base)
	err = fn(ctx, client)
	r.logger.Debug("a provisioner call ended",
		slog.String("type", call.Type.ID),
		slog.String("entry", call.Entry.Name),
		slog.Duration("took", time.Since(started)),
		slog.Bool("failed", err != nil),
	)
	return err
}

// writeSecrets writes a call's secrets into its directory, mode 0600, and
// returns the file's path.
func writeSecrets(dir string, secrets map[string]string) (string, error) {
	encoded, err := json.Marshal(secrets)
	if err != nil {
		return "", fmt.Errorf("provision: encoding the secrets: %w", err)
	}
	name := filepath.Join(dir, "secrets.json")
	//nolint:gosec // 0600 is the point: only the provisioner's user reads it.
	if err := os.WriteFile(name, encoded, 0o600); err != nil {
		return "", fmt.Errorf("provision: writing the secrets: %w", err)
	}
	return name, nil
}

// waitForSocket waits until the provisioner serves on its socket, or the process
// ends first.
func waitForSocket(ctx context.Context, socket string, exited <-chan error) error {
	deadline := time.Now().Add(30 * time.Second)
	for {
		var dialer net.Dialer
		conn, err := dialer.DialContext(ctx, "unix", socket)
		if err == nil {
			_ = conn.Close()
			return nil
		}
		select {
		case err := <-exited:
			if err == nil {
				return fmt.Errorf("it exited before it served")
			}
			return fmt.Errorf("it exited: %w", err)
		case <-ctx.Done():
			return ctx.Err()
		default:
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("it did not open %s in time", socket)
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(10 * time.Millisecond):
		}
	}
}

// stop asks a provisioner to exit and kills it if it does not.
func stop(command *exec.Cmd, exited <-chan error) error {
	select {
	case err := <-exited:
		// The call's result is what matters; a provisioner that exits with a
		// status after answering has still answered.
		_ = err
		return nil
	default:
	}
	if err := command.Process.Signal(syscall.SIGTERM); err != nil {
		if killErr := command.Process.Kill(); killErr != nil {
			return fmt.Errorf("provision: stopping the provisioner: %w", killErr)
		}
	}
	select {
	case err := <-exited:
		_ = err
		return nil
	case <-time.After(10 * time.Second):
		if err := command.Process.Kill(); err != nil {
			return fmt.Errorf("provision: killing the provisioner: %w", err)
		}
		last := <-exited
		_ = last
		return nil
	}
}

// lineWriter is an io.Writer for a child's output, which it hands to a sink a
// line at a time.
type lineWriter struct {
	line func(string)
	mu   sync.Mutex
	rest strings.Builder
}

func (w *lineWriter) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	for {
		index := strings.IndexByte(string(p), '\n')
		if index < 0 {
			w.rest.Write(p)
			return len(p), nil
		}
		w.rest.Write(p[:index])
		if w.line != nil {
			w.line(strings.TrimRight(w.rest.String(), "\r"))
		}
		w.rest.Reset()
		p = p[index+1:]
	}
}
