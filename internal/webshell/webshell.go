// Package webshell is the default shell the server serves when no installed
// plugin is one: the Solid app in web/, built by Vite into dist/.
//
// dist/index.html is committed as a placeholder, so `go build` never needs Bun.
// `make web` replaces it with the built app and its hashed assets.
package webshell

import (
	"embed"
	"io/fs"
	"mime"
	"net/http"
	"path"
	"strings"
)

// dist is the shell's build. The placeholder is committed; a built checkout has
// index.html, assets/ and whatever else Vite wrote.
//
//go:embed all:dist
var dist embed.FS

// assetsPrefix is where Vite puts the files whose names carry their hash.
const assetsPrefix = "/assets/"

// Shell serves the default shell's files. Every path outside assets/ answers
// with index.html, so the app's router decides what the path is.
type Shell struct {
	files fs.FS
}

// New serves the embedded build.
func New() *Shell {
	files, err := fs.Sub(dist, "dist")
	if err != nil {
		// The embed directive cannot fail on a directory that exists in the
		// repository, and dist/ is committed.
		panic("webshell: the embedded build has no dist: " + err.Error())
	}
	return &Shell{files: files}
}

// ServeHTTP serves an asset, or the app's index.html.
func (s *Shell) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet && r.Method != http.MethodHead {
		w.Header().Set("Allow", "GET, HEAD")
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}

	name := strings.TrimPrefix(path.Clean(r.URL.Path), "/")
	if strings.HasPrefix(r.URL.Path, assetsPrefix) {
		contents, err := fs.ReadFile(s.files, name)
		if err != nil {
			http.NotFound(w, r)
			return
		}
		if contentType := mime.TypeByExtension(path.Ext(name)); contentType != "" {
			w.Header().Set("Content-Type", contentType)
		}
		// A hash in the name is the whole cache key: this file never changes.
		w.Header().Set("Cache-Control", "public, max-age=31536000, immutable")
		_, _ = w.Write(contents) //nolint:gosec // a file of the shell's own build, under the hash its name carries
		return
	}

	contents, err := fs.ReadFile(s.files, "index.html")
	if err != nil {
		http.Error(w, "The default shell is not built. Run make web.", http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	// index.html names the current build's assets, so a browser checks it every
	// time and gets a new set after an upgrade.
	w.Header().Set("Cache-Control", "no-cache")
	_, _ = w.Write(contents) //nolint:gosec // the shell's own index.html, which is what a shell is
}
