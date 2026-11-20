package edge

import (
	"context"
	"mime"
	"net/http"
	"path"
	"strings"
)

// Plugins is what the edge serves of the installed plugins: their client halves
// at /plugins/<id>/<hash>/, and the shell's index.html at every path no other
// route answers.
type Plugins interface {
	// ClientFile is a file of a plugin's client half, and whether it is there.
	ClientFile(ctx context.Context, id, hash, name string) ([]byte, bool, error)
	// ShellIndex is the installed shell's index.html, and whether there is one.
	ShellIndex(ctx context.Context) ([]byte, bool, error)
}

// registerPlugins serves the client halves' files.
func (e *Edge) registerPlugins(mux *http.ServeMux) {
	if e.deps.Plugins == nil {
		return
	}
	// `/plugins` is a page of the shell; `/plugins/` is where a client half's files are. The mux would redirect
	// the exact path to the subtree, which has no file called "", so the page answers here.
	mux.HandleFunc("GET /plugins", e.fallback)
	mux.HandleFunc("/plugins/", e.pluginFile)
}

// pluginFile serves one file of a plugin's client half.
//
// A file of a client half is not a secret: the login page needs the shell before
// anyone has signed in, and what a half does is gated by the API's own access
// checks.
func (e *Edge) pluginFile(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet && r.Method != http.MethodHead {
		w.Header().Set("Allow", "GET, HEAD")
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}

	// /plugins/<id>/<hash>/<name>
	rest := strings.TrimPrefix(r.URL.Path, "/plugins/")
	pluginID, rest, found := strings.Cut(rest, "/")
	if !found {
		http.NotFound(w, r)
		return
	}
	hash, name, found := strings.Cut(rest, "/")
	if !found || pluginID == "" || hash == "" || name == "" {
		http.NotFound(w, r)
		return
	}

	contents, ok, err := e.deps.Plugins.ClientFile(r.Context(), pluginID, hash, path.Clean(name))
	if err != nil {
		http.Error(w, "This plugin's files cannot be read.", http.StatusInternalServerError)
		return
	}
	if !ok {
		http.NotFound(w, r)
		return
	}

	if contentType := mime.TypeByExtension(path.Ext(name)); contentType != "" {
		w.Header().Set("Content-Type", contentType)
	}
	w.Header().Set("Cache-Control", "public, max-age=31536000, immutable")
	_, _ = w.Write(contents) //nolint:gosec // a file of the plugin's own package, under the hash it was installed with
}

// shellIndex serves the shell when there is one, and says whether it did.
func (e *Edge) shellIndex(w http.ResponseWriter, r *http.Request) bool {
	if e.deps.Plugins == nil {
		return false
	}
	contents, ok, err := e.deps.Plugins.ShellIndex(r.Context())
	if err != nil || !ok {
		return false
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	w.Header().Set("Cache-Control", "no-cache")
	_, _ = w.Write(contents) //nolint:gosec // the shell is a plugin's own page, which is what a shell is
	return true
}

// defaultShell serves the shell Rookery ships, which is what `/` shows until a
// plugin declares `shell: true`.
func (e *Edge) defaultShell(w http.ResponseWriter, r *http.Request) {
	if e.deps.WebShell == nil {
		return
	}
	e.deps.WebShell.ServeHTTP(w, r)
}
