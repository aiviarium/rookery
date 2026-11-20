package link_test

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/user"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/apply"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/config"
	"github.com/aiviarium/rookery/internal/hosts"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/link"
	"github.com/aiviarium/rookery/internal/link/linktest"
	"github.com/aiviarium/rookery/internal/plugins"
	"github.com/aiviarium/rookery/internal/profile"
	"github.com/aiviarium/rookery/internal/seal"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

func TestApplyAndActionJobsReachTheHost(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithTimeout(access.WithPrincipal(t.Context(), access.LocalAdmin()), 12*time.Second)
	defer cancel()
	opened := storetest.New(t)
	targetSSH := linktest.Start(t, startDaemon(t))
	address, port := targetSSH.Address()
	target := link.Target{Name: "forge", Address: address, Port: port, SSHUser: "ops"}
	links := manager(t, opened, target)
	if _, err := links.Contact(ctx, target, link.Credential{Password: linktest.Password}, testLogger(t)); err != nil {
		t.Fatal(err)
	}

	account, err := user.Current()
	if err != nil {
		t.Fatal(err)
	}
	if account.Username == "root" {
		t.Skip("the test requires an unprivileged operator")
	}
	catalogue := hosts.New(opened)
	if _, err := catalogue.Add(ctx, hosts.AddInput{Name: target.Name, Type: profile.TypeSSH, Profile: profile.Profile{
		Type: profile.TypeSSH, Address: target.Address, Port: target.Port, Operator: account.Username,
		Features: []profile.Feature{{Plugin: "fixture", Feature: "greet"}},
	}}); err != nil {
		t.Fatal(err)
	}

	state := t.TempDir()
	release := t.TempDir()
	installer, secrets := installApplyFeature(ctx, t, opened, state)
	for _, name := range []string{"rookeryd", "box"} {
		if err := os.WriteFile(filepath.Join(release, name), []byte("test binary"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	playbook := filepath.Join(state, "ansible-playbook")
	script := "#!/bin/sh\nset -eu\n" +
		"test -s inventory.yml && test -s key && test -s known_hosts && test -s vars.json\n" +
		"grep -q StrictHostKeyChecking=yes inventory.yml\n" +
		"test -e roles/fixture__greet/tasks/main.yml\n" +
		"grep -q 'on_host_greeting' vars.json\n" +
		"grep 'fixture-sealed-greeting' vars.json\n" +
		"printf 'fake Ansible installed the base role\\n'\n"
	if err := os.WriteFile(playbook, []byte(script), 0o700); err != nil { //nolint:gosec // an executable fake for this test
		t.Fatal(err)
	}

	checker := access.NewChecker(opened)
	registry := jobs.NewRegistry()
	if err := registry.RegisterKinds(jobs.CoreKinds()...); err != nil {
		t.Fatal(err)
	}
	runner := jobs.NewRunner(opened, registry, nil, testLogger(t))
	jobService := jobs.NewService(opened, checker, registry, runner, testLogger(t))
	service := hosts.NewService(hosts.Deps{Hosts: catalogue, Checker: checker, Jobs: jobService, Links: links, Version: "test"})
	runtime := hosts.NewRuntime(service)
	if err := service.RegisterOperations(runtime, registry, hosts.OperationOptions{
		Apply: &apply.Runner{StateDir: state, Playbook: playbook}, ReleaseDir: release,
		Plugins: installer, Secrets: secrets,
	}); err != nil {
		t.Fatal(err)
	}
	running := make(chan error, 1)
	runCtx, stop := context.WithCancel(ctx)
	go func() { running <- runner.Run(runCtx) }()
	defer func() {
		stop()
		if err := <-running; err != nil && !errors.Is(err, context.Canceled) {
			t.Errorf("runner shutdown: %v", err)
		}
	}()

	enqueued, err := service.Apply(ctx, connect.NewRequest(&rookeryv1.HostServiceApplyRequest{Name: target.Name}))
	if err != nil {
		t.Fatal(err)
	}
	applyID := enqueued.Msg.GetJob()
	logs := followHostJob(ctx, t, jobService, applyID)
	if !strings.Contains(logs, "fake Ansible installed the base role") || !strings.Contains(logs, "***") || strings.Contains(logs, "fixture-sealed-greeting") {
		t.Fatalf("apply log = %q", logs)
	}
	record, err := catalogue.Get(ctx, target.Name)
	if err != nil || record.Type != profile.TypeManaged || record.AppliedAt == nil || record.AppliedDigest == "" {
		t.Fatalf("applied host = %+v, %v", record, err)
	}
	if _, err := os.Stat(filepath.Join(state, "jobs", applyID)); !os.IsNotExist(err) {
		t.Fatalf("apply work directory remained after the job: %v", err)
	}

	action, err := service.RunAction(ctx, connect.NewRequest(&rookeryv1.HostServiceRunActionRequest{
		Name: target.Name, Action: "status",
	}))
	if err != nil {
		t.Fatal(err)
	}
	if logs := followHostJob(ctx, t, jobService, action.Msg.GetJob()); strings.TrimSpace(logs) == "" {
		t.Fatal("status produced no output")
	}
	checkTerminalAction(ctx, t, service, links, target.Name)
}

func installApplyFeature(ctx context.Context, t *testing.T, opened *store.Store, state string) (*plugins.Installer, *config.Secrets) {
	t.Helper()
	source := t.TempDir()
	manifest := `id: fixture
version: 0.1.0
name: Fixture
secrets: [greeting]
hostFeatures:
  - id: greet
    types: [managed]
    tasks: features/tasks/main.yml
    secrets: [greeting]
    vars:
      language: {type: string, default: en}
    actions:
      - name: on_host_greeting
        user: operator
        onHost: true
        run: printf hello
      - name: interactive_greeting
        user: operator
        terminal: true
        run: 'printf "%s" "$ROOKERY_ARG_GREETING"'
        args: [{name: greeting, required: true}]
`
	if err := os.MkdirAll(filepath.Join(source, "features", "tasks"), 0o700); err != nil {
		t.Fatal(err)
	}
	for name, contents := range map[string]string{
		"plugin.yaml": manifest, "features/tasks/main.yml": "---\n[]\n",
	} {
		if err := os.WriteFile(filepath.Join(source, name), []byte(contents), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	var pack bytes.Buffer
	if _, err := plugins.Pack(ctx, &pack, source); err != nil {
		t.Fatal(err)
	}
	installer := plugins.NewInstaller(plugins.Options{Store: opened, StateDir: state})
	if _, err := installer.Install(ctx, &pack); err != nil {
		t.Fatal(err)
	}
	key, err := seal.New()
	if err != nil {
		t.Fatal(err)
	}
	ring, err := seal.NewRing(key.ID(), key)
	if err != nil {
		t.Fatal(err)
	}
	secrets := config.NewSecrets(opened, ring)
	if _, err := secrets.Set(ctx, "greeting", "plugin/fixture", []byte("fixture-sealed-greeting")); err != nil {
		t.Fatal(err)
	}
	return installer, secrets
}

func checkTerminalAction(ctx context.Context, t *testing.T, service *hosts.Service, links *link.Manager, host string) {
	t.Helper()
	const greeting = `hello; say "welcome"`
	args, err := cborx.Marshal(map[string]string{"greeting": greeting})
	if err != nil {
		t.Fatal(err)
	}
	action, err := service.RunAction(ctx, connect.NewRequest(&rookeryv1.HostServiceRunActionRequest{
		Name: host, Action: "interactive_greeting", ArgsCbor: args,
	}))
	if err != nil {
		t.Fatal(err)
	}
	id := action.Msg.GetTerminal().GetSession().GetId()
	if id == "" || action.Msg.GetJob() != "" {
		t.Fatalf("wanted a terminal, got %v", action.Msg)
	}
	client, release, err := links.Use(ctx, host)
	if err != nil {
		t.Fatal(err)
	}
	defer release()
	stream := client.Sessions.Attach(ctx)
	if err := stream.Send(&hostv1.SessionServiceAttachRequest{Id: id}); err != nil {
		t.Fatal(err)
	}
	if err := stream.CloseRequest(); err != nil {
		t.Fatal(err)
	}
	defer func() { _ = stream.CloseResponse() }()
	var output strings.Builder
	for {
		message, err := stream.Receive()
		if err != nil {
			if errors.Is(err, io.EOF) {
				break
			}
			t.Fatal(err)
		}
		output.Write(message.GetOutput())
	}
	if output.String() != greeting {
		t.Fatalf("action output %q, want %q", output.String(), greeting)
	}

}

func followHostJob(ctx context.Context, t *testing.T, jobsClient *jobs.Service, id string) string {
	t.Helper()
	var lines strings.Builder
	err := jobsClient.Follow(ctx, id, 0, func(line store.RookeryJobLines, done bool, job jobs.Job) error {
		if !done {
			_, _ = fmt.Fprintln(&lines, line.Line)
			return nil
		}
		if job.State != jobs.Succeeded {
			return fmt.Errorf("job %s: %s (%s)", job.ID, job.State, job.Err)
		}
		return nil
	})
	if err != nil {
		t.Fatalf("following job: %v\n%s", err, lines.String())
	}
	return lines.String()
}
