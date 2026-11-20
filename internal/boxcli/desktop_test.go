package boxcli_test

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"image/png"
	"os"
	"os/exec"
	"os/user"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/aiviarium/rookery/internal/agent"
	"github.com/aiviarium/rookery/internal/boxcli"
	"github.com/aiviarium/rookery/internal/x11"
)

// startXvfb runs an X server for a test, skipping the test when Xvfb is missing.
func startXvfb(t *testing.T, width, height int) string {
	t.Helper()

	if _, err := exec.LookPath("Xvfb"); err != nil {
		t.Skip("box: Xvfb is not installed")
	}

	var lastErr error
	for attempt := 0; attempt < 5; attempt++ {
		var random [2]byte
		if _, err := rand.Read(random[:]); err != nil {
			t.Fatalf("choosing a display: %v", err)
		}
		display := fmt.Sprintf(":%d", 200+int(binary.BigEndian.Uint16(random[:])%2000))

		cmd := exec.CommandContext(context.Background(), "Xvfb", display, "-screen", "0", fmt.Sprintf("%dx%dx24", width, height), //nolint:gosec // a test's own X server.
			"-nolisten", "tcp", "-noreset")
		var output bytes.Buffer
		cmd.Stderr = &output
		if err := cmd.Start(); err != nil {
			t.Fatalf("starting Xvfb: %v", err)
		}
		t.Cleanup(func() {
			_ = cmd.Process.Kill()
			_ = cmd.Wait()
		})

		deadline := time.Now().Add(10 * time.Second)
		for time.Now().Before(deadline) {
			conn, err := x11.Open(context.Background(), display)
			if err == nil {
				_ = conn.Close()
				return display
			}
			lastErr = err
			time.Sleep(20 * time.Millisecond)
		}
	}
	t.Fatalf("no Xvfb answered: %v", lastErr)
	return ""
}

// startDesktop runs Xvfb, a daemon whose desktop belongs to the test's account,
// and `box desktop serve` on that display.
func startDesktop(t *testing.T, width, height int) (*daemon, string) {
	t.Helper()

	current, err := user.Current()
	if err != nil {
		t.Fatalf("the test's account: %v", err)
	}
	display := startXvfb(t, width, height)
	daemon := startDaemon(t, func(config *agent.Config) { config.DesktopUser = current.Username })

	out, errOut := &lockedBuffer{}, &lockedBuffer{}
	options := daemon.options(out, errOut)
	ctx, cancel := context.WithCancel(context.Background())
	served := make(chan error, 1)
	go func() { served <- boxcli.Run(ctx, options, []string{"desktop", "serve", "--display", display}) }()
	t.Cleanup(func() {
		cancel()
		select {
		case err := <-served:
			if err != nil {
				t.Errorf("the desktop provider stopped with %v: %s", err, errOut.String())
			}
		case <-time.After(20 * time.Second):
			t.Errorf("the desktop provider did not stop: %s", errOut.String())
		}
	})

	// The provider registers itself once it has the display.
	deadline := time.Now().Add(20 * time.Second)
	for {
		stdout, _, err := daemon.run(t, "services")
		if err != nil {
			t.Fatalf("services: %v", err)
		}
		if strings.Contains(stdout, agent.DesktopProvider) {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("the desktop did not register: %s", errOut.String())
		}
		time.Sleep(10 * time.Millisecond)
	}
	return daemon, display
}

func TestDesktopCommands(t *testing.T) {
	t.Parallel()

	daemon, _ := startDesktop(t, 320, 200)

	stateOut, _, err := daemon.run(t, "desktop", "state")
	if err != nil {
		t.Fatalf("state: %v", err)
	}
	var state struct {
		Available bool   `json:"available"`
		Paused    bool   `json:"paused"`
		Display   string `json:"display"`
		Width     uint32 `json:"width"`
		Height    uint32 `json:"height"`
	}
	if err := json.Unmarshal([]byte(stateOut), &state); err != nil {
		t.Fatalf("state printed %q: %v", stateOut, err)
	}
	if !state.Available || state.Width != 320 || state.Height != 200 {
		t.Fatalf("the desktop's state is %+v", state)
	}

	path := filepath.Join(t.TempDir(), "screen.png")
	if _, _, err := daemon.run(t, "desktop", "screenshot", "--output", path); err != nil {
		t.Fatalf("screenshot: %v", err)
	}
	raw, err := os.ReadFile(path) //nolint:gosec // the test made the path.
	if err != nil {
		t.Fatalf("reading the screenshot: %v", err)
	}
	picture, err := png.Decode(bytes.NewReader(raw))
	if err != nil {
		t.Fatalf("decoding the screenshot: %v", err)
	}
	if bounds := picture.Bounds(); bounds.Dx() != 320 || bounds.Dy() != 200 {
		t.Errorf("the screenshot is %v, want 320x200", bounds)
	}

	for _, args := range [][]string{
		{"desktop", "click", "40", "50"},
		{"desktop", "click", "40", "50", "right"},
		{"desktop", "type", "hello"},
		{"desktop", "key", "ctrl+t"},
		{"desktop", "scroll", "down", "2"},
	} {
		if _, _, err := daemon.run(t, args...); err != nil {
			t.Errorf("%v: %v", args, err)
		}
	}
}

func TestDesktopClipboardAndPause(t *testing.T) {
	t.Parallel()

	daemon, _ := startDesktop(t, 200, 150)

	if _, _, err := daemon.run(t, "desktop", "clipboard", "--set", "from box"); err != nil {
		t.Fatalf("setting the clipboard: %v", err)
	}
	got, _, err := daemon.run(t, "desktop", "clipboard")
	if err != nil {
		t.Fatalf("reading the clipboard: %v", err)
	}
	if got != "from box" {
		t.Errorf("the clipboard is %q", got)
	}

	if _, _, err := daemon.run(t, "desktop", "pause"); err != nil {
		t.Fatalf("pause: %v", err)
	}

	// A screenshot while paused waits for the resume, which arrives as a call of
	// its own.
	path := filepath.Join(t.TempDir(), "paused.png")
	done := make(chan error, 1)
	go func() {
		_, _, err := daemon.run(t, "desktop", "screenshot", "--output", path)
		done <- err
	}()
	select {
	case err := <-done:
		t.Fatalf("a paused screenshot returned: %v", err)
	case <-time.After(300 * time.Millisecond):
	}

	if _, _, err := daemon.run(t, "desktop", "resume"); err != nil {
		t.Fatalf("resume: %v", err)
	}
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("the screenshot after the resume: %v", err)
		}
	case <-time.After(20 * time.Second):
		t.Fatal("the screenshot did not go on after the resume")
	}
	if _, err := os.Stat(path); err != nil {
		t.Errorf("the screenshot is not there: %v", err)
	}
}

// TestDesktopRefused checks that a desktop command on a daemon whose desktop
// belongs to another account says so.
func TestDesktopRefused(t *testing.T) {
	t.Parallel()

	current, err := user.Current()
	if err != nil {
		t.Fatalf("the test's account: %v", err)
	}
	if current.Username == "dev" {
		t.Skip("the test runs as the desktop's account")
	}

	daemon := startDaemon(t, nil)
	_, _, err = daemon.run(t, "desktop", "state")
	var usage *boxcli.UsageError
	if errors.As(err, &usage) {
		t.Fatalf("state failed as usage: %v", err)
	}
	if err == nil {
		t.Fatal("a desktop on another account's host answered")
	}
}
