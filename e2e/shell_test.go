//go:build e2e

package e2e_test

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/aiviarium/rookery/internal/store/storetest"
)

// TestShell drives the default shell in a browser against a real server: the
// setup link signs the first admin in, and the page they land on is the shell.
//
// The spec is Playwright's, in web/shell/e2e. It needs Bun and the browser
// Playwright downloads; without them the test is skipped locally, and in CI it
// fails.
func TestShell(t *testing.T) {
	if _, err := exec.LookPath("bun"); err != nil {
		skipOrFail(t, "bun is not installed")
	}
	if !playwrightInstalled() {
		skipOrFail(t, "Playwright's Chromium is not installed")
	}

	ctx, cancel := context.WithTimeout(t.Context(), 2*time.Minute)
	defer cancel()

	opened := storetest.New(t)
	dir := t.TempDir()
	for _, key := range []string{"XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_RUNTIME_DIR"} {
		t.Setenv(key, dir)
	}
	fixtureDir := fakeAnsible(t)
	t.Setenv("ROOKERY_RELEASE_DIR", fixtureDir)

	site, socket := startServer(ctx, t, opened.URL(), fixtureDir)
	setup := strings.TrimSpace(string(runCLI(ctx, t, nil, "--server=unix://"+socket, "server", "setup-key")))

	spec := filepath.Join("..", "web", "shell")
	command := exec.CommandContext(ctx, "bun", "x", "playwright", "test", "--config", "playwright.config.ts")
	command.Dir = spec
	command.Env = append(os.Environ(),
		"ROOKERY_SHELL_URL="+site,
		"ROOKERY_SETUP_LINK="+setup,
	)
	output, err := command.CombinedOutput()
	if err != nil {
		t.Fatalf("the shell's browser test failed: %v\n%s", err, output)
	}
	if !strings.Contains(string(output), "passed") {
		t.Fatalf("the shell's browser test did not pass:\n%s", output)
	}
}

// playwrightInstalled reports whether Playwright has a browser to run.
func playwrightInstalled() bool {
	home, err := os.UserHomeDir()
	if err != nil {
		return false
	}
	matches, err := filepath.Glob(filepath.Join(home, ".cache", "ms-playwright", "chromium-*"))
	return err == nil && len(matches) > 0
}

// skipOrFail skips a test that cannot run on this machine, and fails it in CI,
// where the tools are installed and their absence is a broken run.
func skipOrFail(t *testing.T, reason string) {
	t.Helper()
	if os.Getenv("CI") != "" {
		t.Fatal(reason)
	}
	t.Skip(reason)
}
