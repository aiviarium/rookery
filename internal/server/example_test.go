package server_test

import (
	"bytes"
	"context"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	"connectrpc.com/connect"

	examplev1 "github.com/aiviarium/rookery/gen/go/example/v1"
	"github.com/aiviarium/rookery/gen/go/example/v1/examplev1connect"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/authn"
	"github.com/aiviarium/rookery/internal/plugins"
	"github.com/aiviarium/rookery/internal/server"
	"github.com/aiviarium/rookery/internal/store/storetest"
	"github.com/aiviarium/rookery/pkg/sdk"
)

func TestExamplePlugin(t *testing.T) {
	t.Parallel()
	if runtime.GOOS != "linux" || (runtime.GOARCH != "amd64" && runtime.GOARCH != "arm64") {
		t.Skip("the fixture serves on Linux amd64 and arm64")
	}
	opened := storetest.New(t)
	archive := exampleArchive(t)
	ctx, cancel := context.WithTimeout(t.Context(), 60*time.Second)
	defer cancel()
	state, err := os.MkdirTemp(os.Getenv("TMPDIR"), "rk-ex-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(state) }) //nolint:gosec // scratch directory made above
	address := freeAddress(t)
	base := "http://" + address
	socket := filepath.Join(state, "rk.sock")
	health, _ := start(t, server.Config{
		DatabaseURL: opened.URL(), DatabaseLockURL: opened.URL(), Listen: address,
		Socket: socket, SocketMode: 0o660, PublicURL: base, StateDir: state,
		SecretKeyFile: filepath.Join(state, "secret.key"), RetentionDays: 90, Version: "dev",
	})
	waitFor(t, health, http.StatusOK)

	local, localBase := sdk.DialSocket(socket)
	defer local.CloseIdleConnections()
	installer := rookeryv1connect.NewPluginServiceClient(local, localBase, connect.WithGRPC())
	upload := installer.Install(ctx)
	for archive.Len() > 0 {
		if err := upload.Send(&rookeryv1.PluginServiceInstallRequest{Chunk: archive.Next(64 << 10)}); err != nil {
			t.Fatal(err)
		}
	}
	installed, err := upload.CloseAndReceive()
	if err != nil || installed.Msg.GetPlugin().GetId() != "example" {
		t.Fatalf("install: %v, %v", installed, err)
	}
	t.Cleanup(func() {
		// The test database cleanup cannot drop role objects outside its database.
		cleanup, cancel := context.WithTimeout(context.WithoutCancel(t.Context()), 5*time.Second)
		defer cancel()
		_, _ = installer.Remove(cleanup, connect.NewRequest(&rookeryv1.PluginServiceRemoveRequest{Id: "example", Purge: true}))
	})

	user, err := authn.NewUsers(opened).Ensure(ctx, "studio@example.test", "Studio", true)
	if err != nil {
		t.Fatal(err)
	}
	token, _, err := authn.NewTokens(opened).Create(ctx, user.ID, "example-test")
	if err != nil {
		t.Fatal(err)
	}
	spaces := rookeryv1connect.NewSpaceServiceClient(http.DefaultClient, base)
	createSpace := connect.NewRequest(&rookeryv1.SpaceServiceCreateRequest{Kind: "notebook", Name: "Test notes"})
	createSpace.Header().Set("Authorization", "Bearer "+token)
	space, err := spaces.Create(ctx, createSpace)
	if err != nil {
		t.Fatal(err)
	}
	spaceID := space.Msg.GetSpace().GetId()
	request := connect.NewRequest(&examplev1.SummarizeRequest{Titles: []string{"First", "Second"}})
	request.Header().Set("Authorization", "Bearer "+token)
	request.Header().Set("Rookery-Space", spaceID)
	client := examplev1connect.NewNoteServiceClient(http.DefaultClient, base+"/plugins/example/rpc")
	result, err := client.Summarize(ctx, request)
	if err != nil || result.Msg.GetSummary() != "2 note(s): First, Second" || result.Msg.GetUser() != user.Email {
		t.Fatalf("summarize: %v, %v", result, err)
	}

	jobClient := rookeryv1connect.NewJobServiceClient(http.DefaultClient, base)
	enqueue := connect.NewRequest(&rookeryv1.JobServiceEnqueueRequest{Kind: "example/reindex", Resource: "space://" + spaceID})
	enqueue.Header().Set("Authorization", "Bearer "+token)
	queued, err := jobClient.Enqueue(ctx, enqueue)
	if err != nil {
		t.Fatal(err)
	}
	waitForExampleJob(ctx, t, jobClient, queued.Msg.GetJob().GetId(), token)

	if _, err := installer.Remove(ctx, connect.NewRequest(&rookeryv1.PluginServiceRemoveRequest{Id: "example", Purge: true})); err != nil {
		t.Fatal(err)
	}
	if _, err := client.Summarize(ctx, request); connect.CodeOf(err) != connect.CodeUnimplemented && connect.CodeOf(err) != connect.CodeNotFound {
		t.Fatalf("procedure still available after removal: %v", err)
	}
}

func waitForExampleJob(ctx context.Context, t *testing.T, client rookeryv1connect.JobServiceClient, job, token string) {
	t.Helper()
	for {
		request := connect.NewRequest(&rookeryv1.JobServiceGetRequest{Job: job})
		request.Header().Set("Authorization", "Bearer "+token)
		response, err := client.Get(ctx, request)
		if err != nil {
			t.Fatal(err)
		}
		run := response.Msg.GetJob()
		switch run.GetState() {
		case "succeeded":
			var result string
			if err := sdk.Unmarshal(run.GetResultCbor(), &result); err != nil || result != "reindexed" {
				t.Fatalf("job result: %q, %v", result, err)
			}
			return
		case "failed", "interrupted", "cancelled":
			t.Fatalf("job ended: %v", run)
		}
		select {
		case <-ctx.Done():
			t.Fatalf("example job did not finish: %v", run)
		case <-time.After(100 * time.Millisecond):
		}
	}
}

func exampleArchive(t *testing.T) *bytes.Buffer {
	t.Helper()
	root := filepath.Join("..", "..")
	dir := t.TempDir()
	if err := os.CopyFS(dir, os.DirFS(filepath.Join(root, "testdata", "plugins", "example"))); err != nil {
		t.Fatal(err)
	}
	bin := filepath.Join(dir, "bin")
	if err := os.Mkdir(bin, 0o750); err != nil {
		t.Fatal(err)
	}
	cmd := exec.CommandContext(t.Context(), "go", "build", "-ldflags=-s -w", "-o", bin+string(filepath.Separator), "./testdata/plugins/example/service", "./testdata/plugins/example/provisioner") //nolint:gosec // builds repository fixtures into a scratch directory
	cmd.Dir = root
	if output, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("build example: %v\n%s", err, output)
	}
	// The test exercises the native binary. Both named files must exist for packing.
	for _, name := range []string{"service", "provisioner"} {
		for _, arch := range []string{"amd64", "arm64"} {
			if err := copyExampleBinary(bin, name, arch); err != nil {
				t.Fatal(err)
			}
		}
		if err := os.Remove(filepath.Join(bin, name)); err != nil {
			t.Fatal(err)
		}
	}
	var archive bytes.Buffer
	if _, err := plugins.Pack(t.Context(), &archive, dir); err != nil {
		t.Fatal(err)
	}
	return &archive
}

func copyExampleBinary(dir, name, arch string) error {
	source, err := os.DirFS(dir).Open(name)
	if err != nil {
		return err
	}
	defer func() { _ = source.Close() }()
	out, err := os.OpenFile(filepath.Join(dir, name+"-"+arch), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o700) //nolint:gosec // artifact inside the test directory
	if err != nil {
		return err
	}
	_, err = io.Copy(out, source)
	if closeErr := out.Close(); closeErr != nil && err == nil {
		return closeErr
	}
	return err
}
