package boxcli_test

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"os"
	"os/user"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/aiviarium/rookery/internal/agent"
	"github.com/aiviarium/rookery/internal/boxcli"
)

// daemon is a rookeryd a test's box talks to.
type daemon struct {
	socket string
	dir    string
}

// startDaemon runs a daemon on two sockets in a short temporary directory.
func startDaemon(t *testing.T, configure func(*agent.Config)) *daemon {
	t.Helper()

	dir := t.TempDir()
	config := agent.Config{
		Version:        "test",
		RootSocket:     filepath.Join(dir, "root.sock"),
		UserSocket:     filepath.Join(dir, "user.sock"),
		ChannelDB:      filepath.Join(dir, "channel.db"),
		ActionsPath:    filepath.Join(dir, "actions.json"),
		DesktopEnvPath: filepath.Join(dir, "desktop.env"),
		Log:            slog.New(slog.DiscardHandler),
	}
	if configure != nil {
		configure(&config)
	}

	server, err := agent.New(context.Background(), config)
	if err != nil {
		t.Fatalf("starting the daemon: %v", err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	served := make(chan error, 1)
	go func() { served <- server.Serve(ctx) }()
	t.Cleanup(func() {
		cancel()
		select {
		case err := <-served:
			if err != nil {
				t.Errorf("stopping the daemon: %v", err)
			}
		case <-time.After(10 * time.Second):
			t.Error("the daemon did not stop")
		}
	})

	return &daemon{socket: config.UserSocket, dir: dir}
}

// options is what a test runs box with.
func (d *daemon) options(stdout, stderr *lockedBuffer) boxcli.Options {
	return boxcli.Options{
		Socket:         d.socket,
		SecretsDir:     filepath.Join(d.dir, "secrets"),
		DesktopEnvPath: filepath.Join(d.dir, "desktop.env"),
		BoxDir:         filepath.Join(d.dir, "box"),
		Version:        "test",
		Environ:        []string{"PATH=" + os.Getenv("PATH"), "SHELL=/bin/sh"},
		Stdin:          strings.NewReader(""),
		Stdout:         stdout,
		Stderr:         stderr,
	}
}

// run runs box and returns what it wrote.
func (d *daemon) run(t *testing.T, args ...string) (stdout, stderr string, err error) {
	t.Helper()

	out, errOut := &lockedBuffer{}, &lockedBuffer{}
	err = boxcli.Run(context.Background(), d.options(out, errOut), args)
	return out.String(), errOut.String(), err
}

// lockedBuffer is a writer a command and a test may share.
type lockedBuffer struct {
	mu     sync.Mutex
	buffer bytes.Buffer
}

func (b *lockedBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buffer.Write(p)
}

func (b *lockedBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buffer.String()
}

func TestVersion(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	stdout, _, err := daemon.run(t, "version")
	if err != nil {
		t.Fatalf("version: %v", err)
	}
	if stdout != "box test\n" {
		t.Errorf("version printed %q", stdout)
	}
}

func TestPublishAndListen(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	out, errOut := &lockedBuffer{}, &lockedBuffer{}
	options := daemon.options(out, errOut)

	ctx, cancel := context.WithCancel(context.Background())
	listened := make(chan error, 1)
	current, err := user.Current()
	if err != nil {
		t.Fatalf("the test's account: %v", err)
	}
	prefix := "user/" + current.Username + "/"

	go func() { listened <- boxcli.Run(ctx, options, []string{"listen", prefix}) }()

	// Wait for the follower to be there before publishing, because a message
	// kept as none reaches only followers that already are.
	readyTopic := prefix + "ready"
	progressTopic := prefix + "progress"

	deadline := time.Now().Add(20 * time.Second)
	for {
		servicesOut, _, err := daemon.run(t, "publish", readyTopic, `{"ready":true}`, "--keep", "log")
		if err != nil {
			t.Fatalf("publishing: %v", err)
		}
		if strings.TrimSpace(servicesOut) != "" {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("the follower never arrived")
		}
		time.Sleep(5 * time.Millisecond)
	}

	sequence, _, err := daemon.run(t, "publish", progressTopic, `{"step":2,"of":5}`, "--keep", "none")
	if err != nil {
		t.Fatalf("publishing: %v", err)
	}
	if strings.TrimSpace(sequence) == "" {
		t.Fatal("publish printed no sequence")
	}

	deadline = time.Now().Add(20 * time.Second)
	for !strings.Contains(out.String(), `"step":2`) {
		if time.Now().After(deadline) {
			t.Fatalf("the listener printed %q", out.String())
		}
		time.Sleep(5 * time.Millisecond)
	}

	cancel()
	if err := <-listened; err != nil && !errors.Is(err, context.Canceled) {
		t.Fatalf("listen: %v", err)
	}

	line := lastLine(t, out.String())
	var message struct {
		Topic string `json:"topic"`
		Value struct {
			Step int `json:"step"`
			Of   int `json:"of"`
		} `json:"value"`
		Sequence uint64 `json:"sequence"`
	}
	if err := json.Unmarshal([]byte(line), &message); err != nil {
		t.Fatalf("the listener's line %q: %v", line, err)
	}
	if message.Topic != progressTopic || message.Value.Step != 2 || message.Value.Of != 5 {
		t.Errorf("the listener printed %+v", message)
	}
}

// lastLine is the last non-empty line of output.
func lastLine(t *testing.T, output string) string {
	t.Helper()

	lines := strings.Split(strings.TrimSpace(output), "\n")
	if len(lines) == 0 || lines[len(lines)-1] == "" {
		t.Fatalf("there is no line in %q", output)
	}
	return lines[len(lines)-1]
}

func TestActions(t *testing.T) {
	t.Parallel()

	path := filepath.Join(t.TempDir(), "actions.json")
	const actions = `{"actions": [
		{"name": "say", "label": "Say", "user": "root",
		 "command": "printf '%s' \"$ROOKERY_ARG_WHO\"",
		 "arguments": [{"name": "who", "default": "world"}]},
		{"name": "fail", "label": "Fail", "user": "root", "command": "exit 4"}
	]}`
	if err := os.WriteFile(path, []byte(actions), 0o600); err != nil {
		t.Fatalf("writing the actions: %v", err)
	}

	daemon := startDaemon(t, func(config *agent.Config) { config.ActionsPath = path })

	stdout, _, err := daemon.run(t, "actions")
	if err != nil {
		t.Fatalf("actions: %v", err)
	}
	var listed []struct {
		Name      string `json:"name"`
		User      string `json:"user"`
		Arguments []struct {
			Name    string `json:"name"`
			Default string `json:"default"`
		} `json:"arguments"`
	}
	if err := json.Unmarshal([]byte(stdout), &listed); err != nil {
		t.Fatalf("actions printed %q: %v", stdout, err)
	}
	if len(listed) != 2 || listed[1].Name != "say" || listed[1].Arguments[0].Default != "world" {
		t.Fatalf("actions listed %+v", listed)
	}

	stdout, _, err = daemon.run(t, "run", "say", "who=there")
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	if stdout != "there" {
		t.Errorf("run printed %q", stdout)
	}

	stdout, _, err = daemon.run(t, "run", "say")
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	if stdout != "world" {
		t.Errorf("run printed %q, want the default", stdout)
	}

	_, _, err = daemon.run(t, "run", "fail")
	var exit *boxcli.ExitError
	if !errors.As(err, &exit) || exit.ExitCode() != 4 {
		t.Fatalf("a failing action gave %v, want exit 4", err)
	}
}

func TestHostService(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	command := filepath.Join(dir, "echo")
	writeScript(t, command, "#!/bin/sh\ncat\n")

	daemon := startDaemon(t, nil)
	out, errOut := &lockedBuffer{}, &lockedBuffer{}
	options := daemon.options(out, errOut)

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	served := make(chan error, 1)
	go func() { served <- boxcli.Run(ctx, options, []string{"serve", "example/echo", "--", command}) }()

	deadline := time.Now().Add(20 * time.Second)
	for {
		stdout, _, err := daemon.run(t, "services")
		if err != nil {
			t.Fatalf("services: %v", err)
		}
		if strings.Contains(stdout, "example/echo") {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("the service never registered: %q", stdout)
		}
		time.Sleep(5 * time.Millisecond)
	}

	stdout, _, err := daemon.run(t, "call", "example/echo", "greet", `{"hello":"there"}`)
	if err != nil {
		t.Fatalf("call: %v", err)
	}
	var answer map[string]string
	if err := json.Unmarshal([]byte(stdout), &answer); err != nil {
		t.Fatalf("call printed %q: %v", stdout, err)
	}
	if answer["hello"] != "there" {
		t.Errorf("the service answered %+v", answer)
	}

	cancel()
	select {
	case err := <-served:
		if err != nil && !errors.Is(err, context.Canceled) {
			t.Fatalf("serve: %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("serve did not stop")
	}
}

func TestSecret(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	if err := os.MkdirAll(filepath.Join(daemon.dir, "secrets"), 0o700); err != nil {
		t.Fatalf("making the secrets directory: %v", err)
	}
	path := filepath.Join(daemon.dir, "secrets", "example_token")
	if err := os.WriteFile(path, []byte("s3cret"), 0o600); err != nil {
		t.Fatalf("writing the secret: %v", err)
	}

	stdout, _, err := daemon.run(t, "secret", "example_token")
	if err != nil {
		t.Fatalf("secret: %v", err)
	}
	if stdout != "s3cret" {
		t.Errorf("secret printed %q", stdout)
	}

	if _, _, err := daemon.run(t, "secret", "../escape"); err == nil {
		t.Error("a path was accepted as a secret's name")
	}
	if _, _, err := daemon.run(t, "secret", "not-there"); err == nil {
		t.Error("a missing secret was printed")
	}
}

func TestExec(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	if err := os.WriteFile(filepath.Join(daemon.dir, "desktop.env"),
		[]byte("DISPLAY=\":1\"\nXAUTHORITY=/home/dev/.Xauthority\n"), 0o600); err != nil {
		t.Fatalf("writing the desktop's environment: %v", err)
	}

	stdout, _, err := daemon.run(t, "exec", "/bin/sh", "-c", `printf '%s' "$DISPLAY"`)
	if err != nil {
		t.Fatalf("exec: %v", err)
	}
	if stdout != ":1" {
		t.Errorf("exec printed %q, want the desktop's display", stdout)
	}

	_, _, err = daemon.run(t, "exec", "/bin/sh", "-c", "exit 9")
	var exit *boxcli.ExitError
	if !errors.As(err, &exit) || exit.ExitCode() != 9 {
		t.Fatalf("exec gave %v, want exit 9", err)
	}
}

func TestExtension(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	boxDir := filepath.Join(daemon.dir, "box")
	if err := os.MkdirAll(boxDir, 0o750); err != nil {
		t.Fatalf("making the box directory: %v", err)
	}
	writeScript(t, filepath.Join(boxDir, "box-hello"), "#!/bin/sh\nprintf '%s' \"$*\"\n")

	stdout, _, err := daemon.run(t, "hello", "one", "two")
	if err != nil {
		t.Fatalf("hello: %v", err)
	}
	if stdout != "one two" {
		t.Errorf("box hello printed %q", stdout)
	}

	_, _, err = daemon.run(t, "nothing")
	var usage *boxcli.UsageError
	if !errors.As(err, &usage) {
		t.Fatalf("an unknown command gave %v", err)
	}
}

// writeScript writes an executable shell script.
func writeScript(t *testing.T, path, content string) {
	t.Helper()

	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatalf("writing %s: %v", path, err)
	}
	if err := os.Chmod(path, 0o755); err != nil { //nolint:gosec // a test runs it.
		t.Fatalf("making %s executable: %v", path, err)
	}
}
