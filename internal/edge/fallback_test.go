package edge

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// shellStub answers as the default shell does, with a mark that it was reached.
func shellStub(t *testing.T) http.Handler {
	t.Helper()
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		_, _ = w.Write([]byte("the default shell: " + r.URL.Path)) //nolint:gosec // the test's own answer
	})
}

// TestTheDefaultShellAnswersUnmatchedPaths checks that the shell Rookery ships
// takes the paths no route does, and that a shell plugin still goes first.
func TestTheDefaultShellAnswersUnmatchedPaths(t *testing.T) {
	built := &Edge{deps: Deps{WebShell: shellStub(t)}}

	for _, path := range []string{"/", "/plugins/example", "/hosts/studio"} {
		request := httptest.NewRequestWithContext(t.Context(), http.MethodGet, path, nil)
		response := httptest.NewRecorder()
		built.fallback(response, request)

		if response.Code != http.StatusOK {
			t.Fatalf("%s: status %d", path, response.Code)
		}
		if want := "the default shell: " + path; response.Body.String() != want {
			t.Fatalf("%s: body %q, want %q", path, response.Body.String(), want)
		}
	}

	built.deps.Plugins = shellPlugin{index: []byte("<!doctype html><title>A shell</title>")}
	request := httptest.NewRequestWithContext(t.Context(), http.MethodGet, "/hosts/studio", nil)
	response := httptest.NewRecorder()
	built.fallback(response, request)
	if response.Body.String() != "<!doctype html><title>A shell</title>" {
		t.Fatalf("the shell plugin did not go first: %q", response.Body.String())
	}
}

// shellPlugin is an installed plugin whose client half is the shell.
type shellPlugin struct{ index []byte }

func (shellPlugin) ClientFile(context.Context, string, string, string) ([]byte, bool, error) {
	return nil, false, nil
}

func (p shellPlugin) ShellIndex(context.Context) ([]byte, bool, error) {
	return p.index, true, nil
}

// TestNoShellSaysSo checks the page a build without the default shell shows.
func TestNoShellSaysSo(t *testing.T) {
	built := &Edge{}
	request := httptest.NewRequestWithContext(t.Context(), http.MethodGet, "/", nil)
	response := httptest.NewRecorder()
	built.fallback(response, request)

	if response.Code != http.StatusOK {
		t.Fatalf("status %d", response.Code)
	}
	if !strings.Contains(response.Body.String(), "No shell is installed") {
		t.Fatalf("body %q", response.Body.String())
	}
}

// TestTheSignInConfigSaysWhichWaysIn checks the answer a sign-in page draws
// its buttons from.
func TestTheSignInConfigSaysWhichWaysIn(t *testing.T) {
	built := &Edge{}
	request := httptest.NewRequestWithContext(t.Context(), http.MethodGet, "/auth/config", nil)
	response := httptest.NewRecorder()
	built.config(response, request)

	if response.Code != http.StatusOK {
		t.Fatalf("status %d", response.Code)
	}
	if body := response.Body.String(); !strings.Contains(body, `"identityProvider":false`) {
		t.Fatalf("body %q", body)
	}
}
