package plugins_test

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"testing/fstest"

	"github.com/aiviarium/rookery/internal/plugins"
)

// registry is a directory of packages and its key.
type registry struct {
	dir string
	key ed25519.PrivateKey
}

// newRegistry writes the packages into a directory of the test's making, under
// the names a registry serves them at.
func newRegistry(t *testing.T, packages map[string]fstest.MapFS) *registry {
	t.Helper()

	_, key, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("generating a key: %v", err)
	}
	dir := t.TempDir()
	for name, content := range packages {
		plugin := writePackage(t, content)
		data, _ := pack(t, plugin)
		//nolint:gosec // a registry serves its packages to everyone.
		if err := os.WriteFile(filepath.Join(dir, name), data, 0o644); err != nil {
			t.Fatalf("writing %s: %v", name, err)
		}
	}
	return &registry{dir: dir, key: key}
}

// versioned is a package of a plugin at a version.
func versioned(id, version string) fstest.MapFS {
	return fstest.MapFS{
		"plugin.yaml": {
			Data: []byte("id: " + id + "\nversion: " + version + "\nname: " + strings.ToUpper(id) + "\n"),
			Mode: 0o644,
		},
	}
}

func TestIndexDir(t *testing.T) {
	t.Parallel()

	registry := newRegistry(t, map[string]fstest.MapFS{
		"example-0.1.0.tar.gz": versioned("example", "0.1.0"),
		"example-0.2.0.tar.gz": versioned("example", "0.2.0"),
		"other-1.0.0.tar.gz":   versioned("other", "1.0.0"),
	})

	index, signature, err := plugins.IndexDir(context.Background(), registry.dir, registry.key)
	if err != nil {
		t.Fatalf("IndexDir() = %v", err)
	}
	if len(signature) != ed25519.SignatureSize {
		t.Fatalf("the signature is %d bytes", len(signature))
	}

	decoded, err := plugins.LoadIndex(index, signature, registry.key.Public().(ed25519.PublicKey))
	if err != nil {
		t.Fatalf("LoadIndex() = %v", err)
	}
	if decoded.Version != plugins.IndexFormat {
		t.Errorf("the index is version %d", decoded.Version)
	}
	if len(decoded.Plugins) != 2 {
		t.Fatalf("the index lists %+v", decoded.Plugins)
	}
	// Plugins are in id order, and their versions oldest first.
	if decoded.Plugins[0].ID != "example" || decoded.Plugins[1].ID != "other" {
		t.Errorf("the index's order is %+v", decoded.Plugins)
	}
	versions := decoded.Plugins[0].Versions
	if len(versions) != 2 || versions[0].Version != "0.1.0" || versions[1].Version != "0.2.0" {
		t.Errorf("example's versions are %+v", versions)
	}
	if versions[1].URL != "example-0.2.0.tar.gz" {
		t.Errorf("URL = %q", versions[1].URL)
	}
	if len(versions[1].SHA256) != 64 || versions[1].Size == 0 {
		t.Errorf("the version has no hash or size: %+v", versions[1])
	}

	// Resolving takes an exact version, or the newest.
	found, err := decoded.Resolve("example@0.1.0")
	if err != nil {
		t.Fatalf("Resolve() = %v", err)
	}
	if found.Version != "0.1.0" {
		t.Errorf("Resolve(example@0.1.0) = %s", found.Version)
	}
	newest, err := decoded.Resolve("example")
	if err != nil {
		t.Fatalf("Resolve(example) = %v", err)
	}
	if newest.Version != "0.2.0" {
		t.Errorf("Resolve(example) = %s, want the newest", newest.Version)
	}

	// The package the index names is the one whose hash it holds.
	data, err := os.ReadFile(filepath.Join(registry.dir, newest.URL))
	if err != nil {
		t.Fatalf("reading the package: %v", err)
	}
	unpacked, err := plugins.Unpack(context.Background(), bytes.NewReader(data), filepath.Join(t.TempDir(), "out"))
	if err != nil {
		t.Fatalf("Unpack() = %v", err)
	}
	if unpacked.SHA256 != newest.SHA256 {
		t.Errorf("the package's hash is %s and the index says %s", unpacked.SHA256, newest.SHA256)
	}
}

func TestIndexDirIsStable(t *testing.T) {
	t.Parallel()

	registry := newRegistry(t, map[string]fstest.MapFS{
		"example-0.1.0.tar.gz": versioned("example", "0.1.0"),
	})
	first, _, err := plugins.IndexDir(context.Background(), registry.dir, registry.key)
	if err != nil {
		t.Fatalf("IndexDir() = %v", err)
	}
	second, _, err := plugins.IndexDir(context.Background(), registry.dir, registry.key)
	if err != nil {
		t.Fatalf("IndexDir() = %v", err)
	}
	if !bytes.Equal(first, second) {
		t.Error("two indexes of one directory differ")
	}
}

func TestIndexDirRefusesTwoOfOneVersion(t *testing.T) {
	t.Parallel()

	registry := newRegistry(t, map[string]fstest.MapFS{
		"example-a.tar.gz": versioned("example", "0.1.0"),
		"example-b.tar.gz": versioned("example", "0.1.0"),
	})
	if _, _, err := plugins.IndexDir(context.Background(), registry.dir, registry.key); !errors.Is(err, plugins.ErrInvalid) {
		t.Fatalf("IndexDir() = %v, want a refusal", err)
	}
}

func TestVerifyIndex(t *testing.T) {
	t.Parallel()

	registry := newRegistry(t, map[string]fstest.MapFS{
		"example-0.1.0.tar.gz": versioned("example", "0.1.0"),
	})
	index, signature, err := plugins.IndexDir(context.Background(), registry.dir, registry.key)
	if err != nil {
		t.Fatalf("IndexDir() = %v", err)
	}
	pub := registry.key.Public().(ed25519.PublicKey)

	if err := plugins.Verify(pub, index, signature); err != nil {
		t.Fatalf("Verify() = %v", err)
	}

	// Another registry's key.
	_, other, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("generating a key: %v", err)
	}
	if err := plugins.Verify(other.Public().(ed25519.PublicKey), index, signature); !errors.Is(err, plugins.ErrSignature) {
		t.Errorf("Verify() with another key = %v, want a refusal", err)
	}
	// An index changed after signing.
	changed := bytes.Replace(index, []byte("0.1.0"), []byte("0.1.1"), 1)
	if err := plugins.Verify(pub, changed, signature); !errors.Is(err, plugins.ErrSignature) {
		t.Errorf("Verify(a changed index) = %v, want a refusal", err)
	}
	// A signature that is not one.
	if err := plugins.Verify(pub, index, []byte("nope")); !errors.Is(err, plugins.ErrSignature) {
		t.Errorf("Verify(a short signature) = %v, want a refusal", err)
	}
	if _, err := plugins.Sign([]byte("short"), index); !errors.Is(err, plugins.ErrSignature) {
		t.Errorf("Sign(short key) = %v, want a refusal", err)
	}
}

func TestDecodeIndexRefuses(t *testing.T) {
	t.Parallel()

	good := plugins.Index{
		Version: plugins.IndexFormat,
		Plugins: []plugins.IndexPlugin{{
			ID: "example",
			Versions: []plugins.IndexVersion{{
				Version: "0.1.0",
				URL:     "example-0.1.0.tar.gz",
				SHA256:  strings.Repeat("a", 64),
				Size:    10,
			}},
		}},
	}
	encoded, err := plugins.EncodeIndex(good)
	if err != nil {
		t.Fatalf("EncodeIndex() = %v", err)
	}
	if _, err := plugins.DecodeIndex(encoded); err != nil {
		t.Fatalf("DecodeIndex() = %v", err)
	}

	cases := map[string]string{
		"not JSON":               "{",
		"a version from later":   `{"version": 9, "plugins": []}`,
		"an id that is not one":  `{"version": 1, "plugins": [{"id": "Example", "versions": []}]}`,
		"no URL":                 `{"version": 1, "plugins": [{"id": "example", "versions": [{"version": "0.1.0", "sha256": "` + strings.Repeat("a", 64) + `"}]}]}`,
		"no hash":                `{"version": 1, "plugins": [{"id": "example", "versions": [{"version": "0.1.0", "url": "x"}]}]}`,
		"a hash that is not hex": `{"version": 1, "plugins": [{"id": "example", "versions": [{"version": "0.1.0", "url": "x", "sha256": "` + strings.Repeat("z", 64) + `"}]}]}`,
		"two of one plugin":      `{"version": 1, "plugins": [{"id": "example", "versions": []}, {"id": "example", "versions": []}]}`,
	}
	for name, body := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			if _, err := plugins.DecodeIndex([]byte(body)); !errors.Is(err, plugins.ErrInvalid) {
				t.Fatalf("DecodeIndex() = %v, want a refusal", err)
			}
		})
	}

	// A field this build does not know is ignored, so a newer registry's index
	// still works.
	var raw map[string]any
	if err := json.Unmarshal(encoded, &raw); err != nil {
		t.Fatalf("decoding the index: %v", err)
	}
	raw["somethingNew"] = true
	withExtra, err := json.Marshal(raw)
	if err != nil {
		t.Fatalf("encoding the index: %v", err)
	}
	if _, err := plugins.DecodeIndex(withExtra); err != nil {
		t.Fatalf("DecodeIndex(a newer index) = %v", err)
	}
}

func TestResolveRefuses(t *testing.T) {
	t.Parallel()

	index := plugins.Index{
		Version: plugins.IndexFormat,
		Plugins: []plugins.IndexPlugin{{
			ID: "example",
			Versions: []plugins.IndexVersion{{
				Version: "0.1.0", URL: "x", SHA256: strings.Repeat("a", 64),
			}},
		}},
	}
	for name, reference := range map[string]string{
		"a plugin the registry has not": "missing",
		"a version it has not":          "example@9.9.9",
		"not an id":                     "Example",
		"a version that is not one":     "example@latest",
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			if _, err := index.Resolve(reference); err == nil {
				t.Fatalf("Resolve(%q) found something", reference)
			}
		})
	}
}
