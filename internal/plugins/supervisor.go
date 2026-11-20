package plugins

import (
	"bufio"
	"context"
	"crypto/rand"
	"crypto/subtle"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"gopkg.in/yaml.v3"
)

// DefaultServiceMemoryMB is how much resident memory a plugin's service may use
// before the server restarts it, unless ROOKERY_SERVICE_MEMORY_MB says otherwise.
const DefaultServiceMemoryMB = 512

// Supervisor runs the plugins' services: it starts them, restarts them when they
// exit, and stops them on remove, disable and shutdown.
type Supervisor struct {
	source   PluginSource
	stateDir string
	socket   string
	logger   *slog.Logger
	memoryMB int64

	mu      sync.Mutex
	running map[string]*supervised
}

// PluginSource is where the supervisor reads the installed plugins from.
type PluginSource interface {
	List(ctx context.Context) ([]Plugin, error)
	Dir(id, sha string) string
}

// SupervisorOptions are what a supervisor is made of.
type SupervisorOptions struct {
	// Source is the installed plugins.
	Source PluginSource
	// StateDir is ROOKERY_STATE_DIR: the services' sockets are under its
	// plugins/run.
	StateDir string
	// Socket is the server's own Unix socket, ROOKERY_SOCKET to the service.
	Socket string
	// Logger says what a service did.
	Logger *slog.Logger
	// MemoryMB overrides DefaultServiceMemoryMB.
	MemoryMB int64
}

// NewSupervisor returns a supervisor.
func NewSupervisor(options SupervisorOptions) *Supervisor {
	logger := options.Logger
	if logger == nil {
		logger = slog.New(slog.DiscardHandler)
	}
	memoryMB := options.MemoryMB
	if memoryMB == 0 {
		memoryMB = DefaultServiceMemoryMB
		if value, err := strconv.ParseInt(os.Getenv("ROOKERY_SERVICE_MEMORY_MB"), 10, 64); err == nil && value > 0 {
			memoryMB = value
		}
	}
	return &Supervisor{
		source:   options.Source,
		stateDir: options.StateDir,
		socket:   options.Socket,
		logger:   logger,
		memoryMB: memoryMB,
		running:  make(map[string]*supervised),
	}
}

// supervised is one running service.
type supervised struct {
	id     string
	socket string
	token  string

	mu   sync.Mutex
	cmd  *exec.Cmd
	done chan struct{}
	stop bool
}

// Running says whether a service is running.
func (s *Supervisor) Running(id string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	_, ok := s.running[id]
	return ok
}

// Socket is a running service's socket on ROOKERY_PLUGIN_SOCKET, and whether it
// is running.
func (s *Supervisor) Socket(id string) (string, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	service, ok := s.running[id]
	if !ok {
		return "", false
	}
	return service.socket, true
}

// Token is the token a running service calls the server with.
func (s *Supervisor) Token(id string) (string, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	service, ok := s.running[id]
	if !ok {
		return "", false
	}
	return service.token, true
}

// StartAll starts the services whose manifest says "always". A service that
// fails to start is logged and tried again with backoff, as any exit is.
func (s *Supervisor) StartAll(ctx context.Context) {
	plugins, err := s.source.List(ctx)
	if err != nil {
		s.logger.Error("listing the plugins to start their services", slog.String("error", err.Error()))
		return
	}
	for _, plugin := range plugins {
		if !plugin.Enabled {
			continue
		}
		manifest, err := pluginManifest(plugin)
		if err != nil || manifest.Service == nil {
			continue
		}
		if manifest.Service.Start == StartLazy {
			continue
		}
		if err := s.Start(ctx, plugin); err != nil {
			s.logger.Error("starting a plugin's service",
				slog.String("plugin", plugin.ID), slog.String("error", err.Error()))
		}
	}
}

// Ensure starts a service if it is not running, and returns its socket. It is
// what a lazy service's first call does, and what the edge's proxy calls.
func (s *Supervisor) Ensure(ctx context.Context, id string) (string, error) {
	if socket, ok := s.Socket(id); ok {
		return socket, nil
	}
	plugin, err := s.get(ctx, id)
	if err != nil {
		return "", err
	}
	if err := s.Start(context.WithoutCancel(ctx), plugin); err != nil {
		return "", err
	}

	// The process is up when its socket is: it creates the socket before it
	// serves, and the caller's request waits for that, not for a fixed time.
	socket, _ := s.Socket(id)
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(socket); err == nil {
			return socket, nil
		}
		if !s.Running(id) {
			break
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-time.After(20 * time.Millisecond):
		}
	}
	return "", fmt.Errorf("%w: the service of %s did not open its socket", ErrPlugin, id)
}

// Start runs a plugin's service, and keeps it running until its context ends or
// it is stopped. It returns at once: the process is supervised in the
// background.
func (s *Supervisor) Start(ctx context.Context, plugin Plugin) error {
	manifest, err := pluginManifest(plugin)
	if err != nil {
		return err
	}
	if manifest.Service == nil {
		return fmt.Errorf("%w: %s has no service", ErrPlugin, plugin.ID)
	}

	s.mu.Lock()
	if _, ok := s.running[plugin.ID]; ok {
		s.mu.Unlock()
		return nil
	}
	service := &supervised{
		id:     plugin.ID,
		socket: filepath.Join(s.stateDir, "plugins", "run", plugin.ID+".sock"),
		token:  newPluginToken(),
		done:   make(chan struct{}),
	}
	s.running[plugin.ID] = service
	s.mu.Unlock()

	dir := s.source.Dir(plugin.ID, plugin.SHA256)
	go s.supervise(ctx, service, plugin, manifest.Service, dir)
	return nil
}

// Stop ends a plugin's service, and waits for its process to go.
func (s *Supervisor) Stop(id string) {
	s.mu.Lock()
	service, ok := s.running[id]
	delete(s.running, id)
	s.mu.Unlock()
	if !ok {
		return
	}
	service.mu.Lock()
	service.stop = true
	cmd := service.cmd
	service.mu.Unlock()
	if cmd != nil && cmd.Process != nil {
		_ = cmd.Process.Signal(syscall.SIGTERM)
		select {
		case <-service.done:
		case <-time.After(10 * time.Second):
			_ = cmd.Process.Kill()
			<-service.done
		}
	} else {
		<-service.done
	}
	_ = os.Remove(service.socket)
}

// StopAll ends every service, for shutdown.
func (s *Supervisor) StopAll() {
	s.mu.Lock()
	ids := make([]string, 0, len(s.running))
	for id := range s.running {
		ids = append(ids, id)
	}
	s.mu.Unlock()
	var group sync.WaitGroup
	for _, id := range ids {
		group.Add(1)
		go func(id string) {
			defer group.Done()
			s.Stop(id)
		}(id)
	}
	group.Wait()
}

// Restart replaces a plugin's service with the new package's.
func (s *Supervisor) Restart(ctx context.Context, plugin Plugin) error {
	s.Stop(plugin.ID)
	manifest, err := pluginManifest(plugin)
	if err != nil {
		return err
	}
	if manifest.Service == nil || manifest.Service.Start == StartLazy {
		return nil
	}
	return s.Start(ctx, plugin)
}

// supervise runs a service until it is stopped, restarting it with backoff when
// it exits.
func (s *Supervisor) supervise(ctx context.Context, service *supervised, plugin Plugin, declared *Service, dir string) {
	defer close(service.done)
	defer s.forget(service)

	backoff := 250 * time.Millisecond
	const maxBackoff = 30 * time.Second

	for {
		if stopped(service) || ctx.Err() != nil {
			return
		}
		started := time.Now()
		cmd, err := s.command(ctx, service, plugin, declared, dir)
		if err != nil {
			s.logger.Error("starting a plugin's service",
				slog.String("plugin", plugin.ID), slog.String("error", err.Error()))
		} else {
			service.mu.Lock()
			service.cmd = cmd
			service.mu.Unlock()

			exited := make(chan struct{})
			var waitErr error
			go func() { waitErr = cmd.Wait(); close(exited) }()

			stopWatching := s.watchMemory(service, cmd, exited)
			select {
			case <-exited:
				if ctx.Err() != nil || stopped(service) {
					close(stopWatching)
					return
				}
				s.logger.Warn("a plugin's service exited",
					slog.String("plugin", plugin.ID), slog.String("error", errorString(waitErr)))
			case <-ctx.Done():
				close(stopWatching)
				s.terminate(cmd)
				<-exited
				return
			}
			close(stopWatching)
			if time.Since(started) > 10*time.Second {
				backoff = 250 * time.Millisecond
			}
		}

		select {
		case <-ctx.Done():
			return
		case <-time.After(backoff):
		}
		backoff *= 2
		if backoff > maxBackoff {
			backoff = maxBackoff
		}
	}
}

// forget takes a service out of the running set, unless a new one replaced it.
func (s *Supervisor) forget(service *supervised) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if current, ok := s.running[service.id]; ok && current == service {
		delete(s.running, service.id)
	}
}

// watchMemory restarts a service whose resident memory passes the cap. The
// returned channel stops the watch.
func (s *Supervisor) watchMemory(service *supervised, cmd *exec.Cmd, exited chan struct{}) chan struct{} {
	stopped := make(chan struct{})
	go func() {
		ticker := time.NewTicker(2 * time.Second)
		defer ticker.Stop()
		for {
			select {
			case <-stopped:
				return
			case <-exited:
				return
			case <-ticker.C:
				rss, err := residentKB(cmd.Process.Pid)
				if err != nil {
					continue
				}
				if rss > s.memoryMB*1024 {
					s.logger.Warn("a plugin's service passed its memory cap, restarting it",
						slog.String("plugin", service.id), slog.Int64("resident_kb", rss))
					_ = cmd.Process.Kill()
					return
				}
			}
		}
	}()
	return stopped
}

// command builds the process for a service: the platform's binary, or the
// runtime's entry, with an empty environment but the allowed variables.
func (s *Supervisor) command(ctx context.Context, service *supervised, plugin Plugin, declared *Service, dir string) (*exec.Cmd, error) {
	if err := os.MkdirAll(filepath.Dir(service.socket), 0o700); err != nil {
		return nil, fmt.Errorf("plugins: making the run directory: %w", err)
	}
	// A socket an exited process left behind is not the running one.
	_ = os.Remove(service.socket)

	var name string
	var args []string
	switch {
	case declared.Runtime != "":
		if declared.Runtime != "bun" {
			return nil, fmt.Errorf("%w: %s runs its service with %q, which is not known", ErrPlugin, plugin.ID, declared.Runtime)
		}
		bun, err := exec.LookPath("bun")
		if err != nil {
			return nil, fmt.Errorf("%w: bun is not on PATH", ErrPlugin)
		}
		name, args = bun, []string{filepath.Join(dir, declared.Entry)}
	case len(declared.Command) > 0:
		platform := runtime.GOOS + "/" + runtime.GOARCH
		relative, ok := declared.Command[platform]
		if !ok {
			return nil, fmt.Errorf("%w: %s has no service for %s", ErrPlugin, plugin.ID, platform)
		}
		name = filepath.Join(dir, relative)
	default:
		return nil, fmt.Errorf("%w: %s declares no way to run its service", ErrPlugin, plugin.ID)
	}

	//nolint:gosec // the package's own executable, which is what a service is
	cmd := exec.CommandContext(ctx, name, args...)
	cmd.Dir = dir
	cmd.Env = serviceEnvironment(s.socket, service.socket, service.token)
	cmd.Stdout = newLogWriter(s.logger, plugin.ID, slog.LevelInfo)
	cmd.Stderr = newLogWriter(s.logger, plugin.ID, slog.LevelWarn)
	// The server stops a service with SIGTERM, and the process cleans up.
	cmd.Cancel = func() error { return cmd.Process.Signal(syscall.SIGTERM) }
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("plugins: starting %s's service: %w", plugin.ID, err)
	}
	return cmd, nil
}

// serviceEnvironment is what a service runs with: nothing of the server's, but
// these variables.
func serviceEnvironment(serverSocket, pluginSocket, token string) []string {
	env := make([]string, 0, 8)
	for _, name := range []string{"PATH", "HOME", "LANG", "TZ"} {
		if value, ok := os.LookupEnv(name); ok {
			env = append(env, name+"="+value)
		}
	}
	return append(env,
		"ROOKERY_SOCKET="+serverSocket,
		"ROOKERY_PLUGIN_SOCKET="+pluginSocket,
		"ROOKERY_PLUGIN_TOKEN="+token,
	)
}

// terminate stops a service's process, and kills it if it does not go.
func (s *Supervisor) terminate(cmd *exec.Cmd) {
	if cmd == nil || cmd.Process == nil {
		return
	}
	_ = cmd.Process.Signal(syscall.SIGTERM)
	go func() {
		time.Sleep(10 * time.Second)
		_ = cmd.Process.Kill()
	}()
}

func stopped(service *supervised) bool {
	service.mu.Lock()
	defer service.mu.Unlock()
	return service.stop
}

// get reads one plugin.
func (s *Supervisor) get(ctx context.Context, id string) (Plugin, error) {
	plugins, err := s.source.List(ctx)
	if err != nil {
		return Plugin{}, fmt.Errorf("plugins: listing the plugins: %w", err)
	}
	for _, plugin := range plugins {
		if plugin.ID == id {
			if !plugin.Enabled {
				return Plugin{}, fmt.Errorf("%w: %s is disabled", ErrPlugin, id)
			}
			return plugin, nil
		}
	}
	return Plugin{}, fmt.Errorf("%w: %s is not installed", ErrPlugin, id)
}

// pluginManifest reads an installed plugin's manifest.
func pluginManifest(plugin Plugin) (*Manifest, error) {
	var manifest Manifest
	if err := yaml.Unmarshal(plugin.Manifest, &manifest); err != nil {
		return nil, fmt.Errorf("plugins: reading %s's manifest: %w", plugin.ID, err)
	}
	return &manifest, nil
}

// newPluginToken makes the secret a service authenticates with.
func newPluginToken() string {
	secret := make([]byte, 32)
	if _, err := rand.Read(secret); err != nil {
		// crypto/rand does not fail in practice; a service without a token is
		// worse than a server that stops.
		panic(fmt.Sprintf("plugins: making a token: %v", err))
	}
	return hex.EncodeToString(secret)
}

// residentKB is a process's resident memory, from /proc.
func residentKB(pid int) (int64, error) {
	file, err := os.Open(filepath.Join("/proc", strconv.Itoa(pid), "status"))
	if err != nil {
		return 0, err
	}
	defer func() { _ = file.Close() }()

	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := scanner.Text()
		if !strings.HasPrefix(line, "VmRSS:") {
			continue
		}
		fields := strings.Fields(line)
		if len(fields) < 2 {
			break
		}
		return strconv.ParseInt(fields[1], 10, 64)
	}
	if err := scanner.Err(); err != nil {
		return 0, err
	}
	return 0, errors.New("plugins: the process has no VmRSS")
}

func errorString(err error) string {
	if err == nil {
		return "it exited"
	}
	return err.Error()
}

// logWriter writes a service's output to the server's log, with its id.
type logWriter struct {
	logger *slog.Logger
	id     string
	level  slog.Level
	mu     sync.Mutex
	buffer []byte
}

func newLogWriter(logger *slog.Logger, id string, level slog.Level) *logWriter {
	return &logWriter{logger: logger, id: id, level: level}
}

// Write logs whole lines, and holds a partial one until its line ends.
func (w *logWriter) Write(data []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.buffer = append(w.buffer, data...)
	for {
		end := strings.IndexByte(string(w.buffer), '\n')
		if end < 0 {
			break
		}
		line := strings.TrimSuffix(string(w.buffer[:end]), "\r")
		w.buffer = w.buffer[end+1:]
		if line != "" {
			w.logger.Log(context.Background(), w.level, line, slog.String("plugin", w.id))
		}
	}
	return len(data), nil
}

var _ io.Writer = (*logWriter)(nil)

// PluginByToken is the plugin whose running service holds a token. It is how the
// server's socket tells a plugin from the local admin.
func (s *Supervisor) PluginByToken(token string) (string, bool) {
	if token == "" {
		return "", false
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	for id, service := range s.running {
		if subtle.ConstantTimeCompare([]byte(service.token), []byte(token)) == 1 {
			return id, true
		}
	}
	return "", false
}
