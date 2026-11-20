package edge

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"
)

// TestThePluginsPageReachesTheShell checks that `/plugins`, a page of the shell,
// is not swallowed by the subtree that serves the client halves' files.
func TestThePluginsPageReachesTheShell(t *testing.T) {
	built := &Edge{deps: Deps{Plugins: noFiles{}, WebShell: shellStub(t)}}
	mux := http.NewServeMux()
	built.registerPlugins(mux)
	mux.HandleFunc("/", built.fallback)

	request := httptest.NewRequestWithContext(t.Context(), http.MethodGet, "/plugins", nil)
	response := httptest.NewRecorder()
	mux.ServeHTTP(response, request)

	if response.Code != http.StatusOK {
		t.Fatalf("status %d", response.Code)
	}
	if want := "the default shell: /plugins"; response.Body.String() != want {
		t.Fatalf("body %q, want %q", response.Body.String(), want)
	}

	// A path under it is still a client half's file: one that is not there is not
	// found rather than the shell.
	request = httptest.NewRequestWithContext(t.Context(), http.MethodGet, "/plugins/example/abc123/index.js", nil)
	response = httptest.NewRecorder()
	mux.ServeHTTP(response, request)
	if response.Code != http.StatusNotFound {
		t.Fatalf("a file of a client half: status %d", response.Code)
	}
}

// noFiles is a plugin whose client half holds nothing, and which is not the shell.
type noFiles struct{}

func (noFiles) ClientFile(context.Context, string, string, string) ([]byte, bool, error) {
	return nil, false, nil
}

func (noFiles) ShellIndex(context.Context) ([]byte, bool, error) {
	return nil, false, nil
}

// halfWithFile is a plugin whose client half holds one file.
type halfWithFile struct{}

func (halfWithFile) ClientFile(_ context.Context, id, hash, name string) ([]byte, bool, error) {
	if id == "example" && hash == "abc123" && name == "index.js" {
		return []byte("export {}"), true, nil
	}
	return nil, false, nil
}

func (halfWithFile) ShellIndex(context.Context) ([]byte, bool, error) {
	return nil, false, nil
}

// TestAClientHalfStillAnswers checks that the exact page route did not take the
// files' path with it.
func TestAClientHalfStillAnswers(t *testing.T) {
	built := &Edge{deps: Deps{Plugins: halfWithFile{}, WebShell: shellStub(t)}}
	mux := http.NewServeMux()
	built.registerPlugins(mux)

	request := httptest.NewRequestWithContext(t.Context(), http.MethodGet, "/plugins/example/abc123/index.js", nil)
	response := httptest.NewRecorder()
	mux.ServeHTTP(response, request)

	if response.Code != http.StatusOK {
		t.Fatalf("status %d", response.Code)
	}
	if response.Body.String() != "export {}" {
		t.Fatalf("body %q", response.Body.String())
	}
}
