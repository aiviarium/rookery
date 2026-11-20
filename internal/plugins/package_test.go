package plugins_test

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"testing"
	"testing/fstest"

	"github.com/aiviarium/rookery/internal/plugins"
)

// writePackage writes a plugin's files into a directory of the test's making.
func writePackage(t *testing.T, content fstest.MapFS) string {
	t.Helper()

	dir := t.TempDir()
	for name, file := range content {
		path := filepath.Join(dir, filepath.FromSlash(name))
		//nolint:gosec // a plugin folder is a directory a role is read from.
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatalf("making %s: %v", path, err)
		}
		mode := file.Mode.Perm()
		if mode == 0 {
			mode = 0o644
		}
		if err := os.WriteFile(path, file.Data, mode); err != nil {
			t.Fatalf("writing %s: %v", path, err)
		}
	}
	return dir
}

// pack packs the content into memory.
func pack(t *testing.T, dir string) ([]byte, plugins.Packed) {
	t.Helper()

	var buffer bytes.Buffer
	packed, err := plugins.Pack(context.Background(), &buffer, dir)
	if err != nil {
		t.Fatalf("Pack() = %v", err)
	}
	return buffer.Bytes(), packed
}

func TestPackWritesAReproduciblePackage(t *testing.T) {
	t.Parallel()

	dir := writePackage(t, exampleContent())
	first, packed := pack(t, dir)

	if packed.Manifest.ID != "example" || packed.Manifest.Version != "0.1.0" {
		t.Errorf("Pack() = %s@%s", packed.Manifest.ID, packed.Manifest.Version)
	}
	sum := sha256.Sum256(first)
	if got, want := packed.SHA256, hex.EncodeToString(sum[:]); got != want {
		t.Errorf("SHA256 = %s, want %s", got, want)
	}
	if packed.Size != int64(len(first)) {
		t.Errorf("Size = %d, want %d", packed.Size, len(first))
	}

	// Packing the same files again gives the same bytes, so a registry's index
	// stays valid when nothing changed.
	second, again := pack(t, dir)
	if !bytes.Equal(first, second) {
		t.Error("two packs of one plugin differ")
	}
	if again.SHA256 != packed.SHA256 {
		t.Errorf("two packs have different hashes: %s and %s", packed.SHA256, again.SHA256)
	}
}

func TestUnpackRoundTrip(t *testing.T) {
	t.Parallel()

	dir := writePackage(t, exampleContent())
	data, packed := pack(t, dir)

	destination := filepath.Join(t.TempDir(), "unpacked")
	unpacked, err := plugins.Unpack(context.Background(), bytes.NewReader(data), destination)
	if err != nil {
		t.Fatalf("Unpack() = %v", err)
	}
	if unpacked.SHA256 != packed.SHA256 {
		t.Errorf("Unpack() hash = %s, want %s", unpacked.SHA256, packed.SHA256)
	}
	if unpacked.Manifest.ID != "example" {
		t.Errorf("Unpack() manifest = %s", unpacked.Manifest.ID)
	}

	// The files are there, with the modes they were packed with.
	for name, mode := range map[string]fs.FileMode{
		"plugin.yaml":                   0o644,
		"bin/service-amd64":             0o755,
		"features/hello/tasks/main.yml": 0o644,
	} {
		info, err := os.Stat(filepath.Join(destination, filepath.FromSlash(name)))
		if err != nil {
			t.Errorf("%s: %v", name, err)
			continue
		}
		if info.Mode().Perm() != mode {
			t.Errorf("%s has mode %o, want %o", name, info.Mode().Perm(), mode)
		}
	}

	// What was unpacked is a package that checks, so a second pack has the same
	// hash.
	again, err := plugins.Check(os.DirFS(destination))
	if err != nil {
		t.Fatalf("Check() of an unpacked package = %v", err)
	}
	if again.ID != "example" {
		t.Errorf("Check() = %+v", again)
	}
}

func TestPackRefusesABrokenPackage(t *testing.T) {
	t.Parallel()

	content := exampleContent()
	delete(content, "bin/service-amd64")
	dir := writePackage(t, content)

	var buffer bytes.Buffer
	if _, err := plugins.Pack(context.Background(), &buffer, dir); !errors.Is(err, plugins.ErrInvalid) {
		t.Fatalf("Pack() = %v, want a refusal", err)
	}
	if _, err := plugins.Pack(context.Background(), &buffer, filepath.Join(dir, "nowhere")); err == nil {
		t.Fatal("Pack() accepted a directory that is not there")
	}
}

func TestUnpackRefuses(t *testing.T) {
	t.Parallel()

	dir := writePackage(t, exampleContent())
	good, _ := pack(t, dir)

	// A byte changed is not the package it was.
	broken := bytes.Clone(good)
	broken[len(broken)/2] ^= 0xff
	if _, err := plugins.Unpack(context.Background(), bytes.NewReader(broken), filepath.Join(t.TempDir(), "x")); !errors.Is(err, plugins.ErrPackage) {
		t.Errorf("Unpack(a changed package) = %v, want a refusal", err)
	}

	// Not a package at all.
	if _, err := plugins.Unpack(context.Background(), bytes.NewReader([]byte("not a package")), filepath.Join(t.TempDir(), "y")); !errors.Is(err, plugins.ErrPackage) {
		t.Errorf("Unpack(not a package) = %v, want a refusal", err)
	}

	// A package without a manifest.
	empty := packEntries(t, map[string]string{"README": "nothing here"})
	if _, err := plugins.Unpack(context.Background(), bytes.NewReader(empty), filepath.Join(t.TempDir(), "z")); !errors.Is(err, plugins.ErrPackage) {
		t.Errorf("Unpack(a package without a manifest) = %v, want a refusal", err)
	}
}

func TestUnpackRefusesPathsThatLeave(t *testing.T) {
	t.Parallel()

	for name, entry := range map[string]string{
		"a path that climbs": "../evil",
		"an absolute path":   "/etc/evil",
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			data := packEntriesAt(t, entry, "id: example\nversion: 1.0.0\nname: E\n")
			destination := filepath.Join(t.TempDir(), "out")
			if _, err := plugins.Unpack(context.Background(), bytes.NewReader(data), destination); !errors.Is(err, plugins.ErrPackage) {
				t.Fatalf("Unpack() = %v, want a refusal", err)
			}
			if _, err := os.Stat(filepath.Join(destination, "..", "evil")); err == nil {
				t.Fatal("the package wrote outside its directory")
			}
		})
	}
}

func TestUnpackRefusesASymlink(t *testing.T) {
	t.Parallel()

	data := packSymlink(t)
	if _, err := plugins.Unpack(context.Background(), bytes.NewReader(data), filepath.Join(t.TempDir(), "out")); !errors.Is(err, plugins.ErrPackage) {
		t.Fatalf("Unpack() = %v, want a refusal", err)
	}
}

func TestManifestOf(t *testing.T) {
	t.Parallel()

	dir := writePackage(t, exampleContent())
	data, _ := pack(t, dir)

	manifest, err := plugins.ManifestOf(bytes.NewReader(data))
	if err != nil {
		t.Fatalf("ManifestOf() = %v", err)
	}
	if manifest.ID != "example" || manifest.Version != "0.1.0" {
		t.Errorf("ManifestOf() = %s@%s", manifest.ID, manifest.Version)
	}
}
