//go:build e2e

package e2e_test

import (
	"bytes"
	"context"
	"fmt"
	"image/color"
	"image/png"
	"io"
	"net"
	"net/http"
	"net/http/cookiejar"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/cli"
	"github.com/aiviarium/rookery/internal/link/linktest"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

func TestCore(t *testing.T) {
	// The CLI writes its device configuration and Ansible looks up its
	// executable in PATH. Both are passed scratch directories in this process.
	ctx, cancel := context.WithTimeout(t.Context(), 20*time.Second)
	defer cancel()
	opened := storetest.New(t)
	dir := t.TempDir()
	for _, key := range []string{"XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_RUNTIME_DIR"} {
		t.Setenv(key, dir)
	}
	authFile := filepath.Join(dir, "Xauthority")
	if err := os.WriteFile(authFile, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("XAUTHORITY", authFile)
	t.Setenv("ROOKERY_SERVER", "")
	t.Setenv("ROOKERY_TOKEN", "")

	fixtureDir := fakeAnsible(t)
	t.Setenv("PATH", fixtureDir+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("ROOKERY_RELEASE_DIR", fixtureDir)

	display := startXvfb(ctx, t)
	x := paintDesktop(t, display)

	studio := startHost(ctx, t, display)
	forge := startHost(ctx, t, "")
	rfb := serveRFB(ctx, t, studio.runtime)

	site, socket := startServer(ctx, t, opened.URL(), fixtureDir)
	browser, token := signIn(ctx, t, site, socket)
	if output := runCLI(ctx, t, strings.NewReader(token+"\n"), "login", site, "--token-stdin"); !strings.Contains(string(output), "operator@example.test") {
		t.Fatalf("CLI sign-in returned %q", output)
	}
	runCLI(ctx, t, nil, "whoami")

	httpClient := grpcClient(t)
	hostAPI := rookeryv1connect.NewHostServiceClient(httpClient, site, connect.WithGRPC())
	for _, host := range []struct {
		name    string
		ssh     *linktest.SSH
		profile rookeryv1.HostType
	}{
		{"studio", studio.ssh, rookeryv1.HostType_HOST_TYPE_WORKSTATION},
		{"forge", forge.ssh, rookeryv1.HostType_HOST_TYPE_MANAGED},
	} {
		address, port := host.ssh.Address()
		runCLI(ctx, t, strings.NewReader(linktest.Password+"\n"), "host", "add", host.name, address, "--port", strconv.Itoa(port), "--password-stdin")
		if key, err := os.ReadFile(host.ssh.AuthorizedKeys()); err != nil || !bytes.Contains(key, []byte("ssh-")) {
			t.Fatalf("first contact with %s did not install the key: %v", host.name, err)
		}
		record, err := hostAPI.Get(ctx, bearer(token, &rookeryv1.HostServiceGetRequest{Name: host.name}))
		if err != nil {
			t.Fatal(err)
		}
		p := record.Msg.GetHost().GetProfile()
		p.Tailnet = false
		p.Type = host.profile
		if host.name == "studio" {
			p.Desktop = &rookeryv1.Desktop{User: studio.user, Width: 320, Height: 200}
		}
		if _, err := hostAPI.SetProfile(ctx, bearer(token, &rookeryv1.HostServiceSetProfileRequest{Name: host.name, Profile: p})); err != nil {
			t.Fatal(err)
		}
	}

	if output := runCLI(ctx, t, nil, "apply", "studio"); !bytes.Contains(output, []byte("fake ansible applied studio")) {
		t.Fatalf("apply did not relay the playbook log: %q", output)
	}
	studioRecord, err := hostAPI.Get(ctx, bearer(token, &rookeryv1.HostServiceGetRequest{Name: "studio"}))
	if err != nil {
		t.Fatal(err)
	}
	applied := studioRecord.Msg.GetHost()
	if applied.GetType() != rookeryv1.HostType_HOST_TYPE_WORKSTATION || applied.GetAppliedDigest() == "" || applied.GetAppliedDigest() != applied.GetDigest() {
		t.Fatalf("apply left studio in the wrong state: %v", applied)
	}

	screenshot := runCLI(ctx, t, nil, "screenshot", "studio", "--width", "160")
	picture, err := png.Decode(bytes.NewReader(screenshot))
	if err != nil {
		t.Fatalf("decoding workstation screenshot: %v", err)
	}
	wantColor := color.NRGBA{R: 0x3a, G: 0x90, B: 0xbc, A: 255}
	if picture.Bounds().Dx() != 160 || color.NRGBAModel.Convert(picture.At(20, 20)) != wantColor || color.NRGBAModel.Convert(picture.At(150, 90)) == wantColor {
		t.Fatalf("screenshot has incorrect dimensions or colors: %v", picture.Bounds())
	}

	desktopAPI := rookeryv1connect.NewDesktopServiceClient(httpClient, site, connect.WithGRPC())
	if _, err := desktopAPI.Input(ctx, bearer(token, &rookeryv1.DesktopServiceInputRequest{
		Host: "studio",
		Input: &rookeryv1.DesktopServiceInputRequest_Click{
			Click: &rookeryv1.DesktopClick{X: 28, Y: 36, Button: rookeryv1.MouseButton_MOUSE_BUTTON_LEFT},
		},
	})); err != nil {
		t.Fatal(err)
	}
	assertPointer(t, x, 28, 36)

	checkBrowserRFB(ctx, t, site, browser)
	checkCLIRFB(ctx, t)
	if err := rfb(); err != nil {
		t.Fatalf("fake VNC: %v", err)
	}

	checkBox(ctx, t, httpClient, site, token, studio)

	rollouts := rookeryv1connect.NewRolloutServiceClient(httpClient, site, connect.WithGRPC())
	checkRollout(ctx, t, token, rollouts)
	record, err := os.ReadFile(filepath.Join(fixtureDir, "applies")) //nolint:gosec // the fixture writes this file in its scratch directory.
	if err != nil {
		t.Fatal(err)
	}
	if names := strings.Fields(string(record)); !slices.Equal(names, []string{"studio", "forge", "studio"}) {
		t.Fatalf("fake Ansible saw the wrong runs: %q", names)
	}
	for _, name := range []string{"studio", "forge"} {
		runCLI(ctx, t, nil, "host", "remove", name)
	}
}

func fakeAnsible(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	// The binaries are only staged by the apply. The actual daemons are already
	// running in this process.
	for _, name := range []string{"rookeryd", "box", "rookeryd-linux-amd64", "box-linux-amd64", "rookeryd-linux-arm64", "box-linux-arm64"} {
		if err := os.WriteFile(filepath.Join(dir, name), []byte("fixture\n"), 0o600); err != nil {
			t.Fatal(err)
		}
	}

	const script = `#!/bin/sh
set -eu
test "$#" -eq 5
test "$1" = -i
test "$4" = -e
test -s "${5#@}"
test -s "$ANSIBLE_CONFIG"
grep -q 'StrictHostKeyChecking=yes' "$2"
grep -q '^    - base$' "$3"
host=$(sed -n '4s/^[[:space:]]*"\([^"]*\)":$/\1/p' "$2")
case "$host" in
  studio) grep -q '^    - desktop$' "$3" ;;
  forge) if grep -q '^    - desktop$' "$3"; then exit 1; fi ;;
  *) exit 1 ;;
esac
printf '%s\n' "$host" >> "$(dirname "$0")/applies"
printf 'fake ansible applied %s\n' "$host"
`
	path := filepath.Join(dir, "ansible-playbook")
	if err := os.WriteFile(path, []byte(script), 0o700); err != nil { //nolint:gosec // a test runs the script.
		t.Fatal(err)
	}
	return dir
}

func runCLI(ctx context.Context, t *testing.T, stdin io.Reader, args ...string) []byte {
	t.Helper()
	root := cli.New("e2e") //nolint:contextcheck // Cobra receives the context at ExecuteContext.
	root.SetArgs(args)
	root.SetIn(stdin)
	var output, diagnostics bytes.Buffer
	root.SetOut(&output)
	root.SetErr(&diagnostics)
	if err := root.ExecuteContext(ctx); err != nil {
		t.Fatalf("rookery %v: %v\n%s%s", args, err, output.Bytes(), diagnostics.Bytes())
	}
	return output.Bytes()
}

func signIn(ctx context.Context, t *testing.T, site, socket string) (*http.Client, string) {
	t.Helper()
	local := "--server=unix://" + socket
	setup := strings.TrimSpace(string(runCLI(ctx, t, nil, local, "server", "setup-key")))
	jar, err := cookiejar.New(nil)
	if err != nil {
		t.Fatal(err)
	}
	client := &http.Client{Jar: jar}
	values := url.Values{"email": {"operator@example.test"}, "name": {"E2E Operator"}}
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, setup, strings.NewReader(values.Encode()))
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	response, err := client.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = response.Body.Close() }()
	if response.StatusCode != http.StatusOK {
		t.Fatalf("signing in with the setup link: %s", response.Status)
	}
	origin, err := url.Parse(site)
	if err != nil {
		t.Fatal(err)
	}
	if len(jar.Cookies(origin)) == 0 {
		t.Fatal("sign-in did not set a browser session")
	}
	token := runCLI(ctx, t, nil, local, "server", "token", "--user", "operator@example.test", "--name", "Fast integration test")
	return client, strings.TrimSpace(string(token))
}

func grpcClient(t *testing.T) *http.Client {
	t.Helper()
	transport := &http.Transport{Protocols: new(http.Protocols)}
	transport.Protocols.SetUnencryptedHTTP2(true)
	t.Cleanup(transport.CloseIdleConnections)
	return &http.Client{Transport: transport}
}

func bearer[T any](token string, message *T) *connect.Request[T] {
	req := connect.NewRequest(message)
	req.Header().Set("Authorization", "Bearer "+token)
	return req
}

func checkRollout(ctx context.Context, t *testing.T, token string, client rookeryv1connect.RolloutServiceClient) {
	t.Helper()
	result, err := client.Start(ctx, bearer(token, &rookeryv1.RolloutServiceStartRequest{
		Kind:      "host.apply",
		Selection: &rookeryv1.RolloutSelection{Hosts: []string{"studio", "forge"}},
		Strategy:  &rookeryv1.RolloutStrategy{BatchSize: 1},
	}))
	if err != nil {
		t.Fatal(err)
	}
	updates, err := client.Watch(ctx, bearer(token, &rookeryv1.RolloutServiceWatchRequest{Rollout: result.Msg.GetRollout().GetId()}))
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = updates.Close() }()
	for updates.Receive() {
		current := updates.Msg().GetRollout()
		if current.GetRunning() > 1 {
			t.Errorf("the batch of one started several hosts: %v", current)
		}
		switch current.GetState() {
		case "succeeded":
			if current.GetSucceeded() != 2 || len(current.GetHosts()) != 2 ||
				current.GetHosts()[0].GetBatch() == current.GetHosts()[1].GetBatch() ||
				current.GetHosts()[0].GetJob() == "" || current.GetHosts()[1].GetJob() == "" {
				t.Errorf("the roll-out did not apply both hosts in separate jobs and batches: %v", current)
			}
			return
		case "paused", "failed", "cancelled":
			t.Fatalf("roll-out %s: %v", current.GetState(), current)
		}
	}
	t.Fatalf("roll-out stream ended before success: %v", updates.Err())
}

func await(ctx context.Context, probe func() bool) error {
	ticker := time.NewTicker(10 * time.Millisecond)
	defer ticker.Stop()
	for {
		if probe() {
			return nil
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("waiting for readiness: %w", ctx.Err())
		case <-ticker.C:
		}
	}
}

func loopback(ctx context.Context, t *testing.T) net.Listener {
	t.Helper()
	l, err := (&net.ListenConfig{}).Listen(ctx, "tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	return l
}
