package webshell

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"testing/fstest"
)

// shellFor serves a made-up build, so the tests do not depend on what the
// checkout's dist holds.
func shellFor() *Shell {
	return &Shell{files: fstest.MapFS{
		"index.html":           {Data: []byte("<!doctype html><title>Rookery</title>")},
		"assets/index-a1b2.js": {Data: []byte("console.log('shell')")},
	}}
}

// TestTheShellAnswersEveryPath checks that the app's router gets a path no
// asset answers, and that an asset is served with the hash's cache header.
func TestTheShellAnswersEveryPath(t *testing.T) {
	shell := shellFor()
	cases := []struct {
		name        string
		method      string
		path        string
		status      int
		body        string
		contentType string
		cache       string
	}{
		{"the root", http.MethodGet, "/", http.StatusOK, "<!doctype html>", "text/html; charset=utf-8", "no-cache"},
		{"a route", http.MethodGet, "/plugins/example", http.StatusOK, "<!doctype html>", "text/html; charset=utf-8", "no-cache"},
		{"an asset", http.MethodGet, "/assets/index-a1b2.js", http.StatusOK, "console.log", "text/javascript; charset=utf-8", "public, max-age=31536000, immutable"},
		{"a missing asset", http.MethodGet, "/assets/index-ffff.js", http.StatusNotFound, "", "", ""},
		{"a write", http.MethodPost, "/", http.StatusMethodNotAllowed, "", "", ""},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			request := httptest.NewRequestWithContext(t.Context(), test.method, test.path, nil)
			response := httptest.NewRecorder()
			shell.ServeHTTP(response, request)

			if response.Code != test.status {
				t.Fatalf("%s %s: status %d, want %d", test.method, test.path, response.Code, test.status)
			}
			if !strings.Contains(response.Body.String(), test.body) {
				t.Fatalf("%s %s: body %q does not hold %q", test.method, test.path, response.Body.String(), test.body)
			}
			if test.contentType != "" && response.Header().Get("Content-Type") != test.contentType {
				t.Fatalf("%s %s: content type %q, want %q", test.method, test.path, response.Header().Get("Content-Type"), test.contentType)
			}
			if test.cache != "" && response.Header().Get("Cache-Control") != test.cache {
				t.Fatalf("%s %s: cache %q, want %q", test.method, test.path, response.Header().Get("Cache-Control"), test.cache)
			}
		})
	}
}

// TestTheCommittedBuildIsAPage checks that a checkout without `make web` still
// has an index.html to embed.
func TestTheCommittedBuildIsAPage(t *testing.T) {
	request := httptest.NewRequestWithContext(t.Context(), http.MethodGet, "/", nil)
	response := httptest.NewRecorder()
	New().ServeHTTP(response, request)

	if response.Code != http.StatusOK {
		t.Fatalf("the embedded build answers %d", response.Code)
	}
	if response.Header().Get("Content-Type") != "text/html; charset=utf-8" {
		t.Fatalf("the embedded build is %q", response.Header().Get("Content-Type"))
	}
}
