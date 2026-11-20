package edge

import (
	"context"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"testing"
	"time"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/plugins"
)

type fakePlugins struct{}

func (fakePlugins) ClientFile(context.Context, string, string, string) ([]byte, bool, error) {
	return nil, false, nil
}
func (fakePlugins) ShellIndex(context.Context) ([]byte, bool, error) { return nil, false, nil }

type fakeServices struct{ socket string }

func (f fakeServices) Ensure(context.Context, string) (string, error) { return f.socket, nil }

type fakeProcedures struct {
	level    plugins.Level
	resource plugins.Resource
	declared bool
}

func (f fakeProcedures) Access(context.Context, string, string) (plugins.Level, plugins.Resource, bool, error) {
	return f.level, f.resource, f.declared, nil
}

// TestTheProxyForwardsTheProcedurePath checks that the edge's prefix goes and
// the procedure's own Connect path stays, that the caller's credentials do not
// follow the call, and that an undeclared procedure is refused before the
// plugin is reached.
func TestTheProxyForwardsTheProcedurePath(t *testing.T) {
	dir := t.TempDir()
	socket := filepath.Join(dir, "plugin.sock")
	var listenConfig net.ListenConfig
	listener, err := listenConfig.Listen(t.Context(), "unix", socket)
	if err != nil {
		t.Fatal(err)
	}

	reached := make(chan string, 1)
	plugin := &http.Server{ReadHeaderTimeout: time.Minute, Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		reached <- r.URL.Path + " " + r.Header.Get("Authorization")
		_, _ = io.WriteString(w, "summary")
	})}
	go func() { _ = plugin.Serve(listener) }()
	t.Cleanup(func() { _ = plugin.Close() })

	build := func(declared bool) *httptest.Server {
		built := &Edge{deps: Deps{
			Plugins:    fakePlugins{},
			Services:   fakeServices{socket: socket},
			Procedures: fakeProcedures{level: plugins.LevelView, resource: plugins.ResourceSpace, declared: declared},
		}}
		mux := http.NewServeMux()
		built.registerPluginRPC(mux)
		return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			ctx := access.WithPrincipal(r.Context(), access.LocalAdmin())
			mux.ServeHTTP(w, r.WithContext(ctx))
		}))
	}

	// A user's privilege comes from the grant chain, which the local admin skips;
	// the admin here is only to reach the proxy without a database.
	server := build(true)
	defer server.Close()
	request, err := http.NewRequestWithContext(t.Context(), http.MethodPost, server.URL+"/plugins/example/rpc/example.v1.NoteService/Summarize", nil)
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Rookery-Space", "01hspace")
	request.Header.Set("Authorization", "Bearer a-user-token")
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = response.Body.Close() }()
	body, _ := io.ReadAll(response.Body)
	if response.StatusCode != http.StatusOK || string(body) != "summary" {
		t.Fatalf("the call answered %d %q", response.StatusCode, body)
	}
	select {
	case seen := <-reached:
		if seen != "/example.v1.NoteService/Summarize " {
			t.Fatalf("the plugin saw %q", seen)
		}
	default:
		t.Fatal("the plugin was not reached")
	}

	undeclared := build(false)
	defer undeclared.Close()
	request, err = http.NewRequestWithContext(t.Context(), http.MethodPost, undeclared.URL+"/plugins/example/rpc/example.v1.NoteService/Summarize", nil)
	if err != nil {
		t.Fatal(err)
	}
	response, err = http.DefaultClient.Do(request) //nolint:gosec // the test's own server
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = response.Body.Close() }()
	if response.StatusCode != http.StatusNotFound {
		t.Fatalf("an undeclared procedure answered %d", response.StatusCode)
	}
}
