package plugins_test

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"testing"
	"time"
)

// packEntries is a .tar.gz of the given files, for the packages a real Pack
// would refuse to make.
func packEntries(t *testing.T, files map[string]string) []byte {
	t.Helper()

	var buffer bytes.Buffer
	compressed := gzip.NewWriter(&buffer)
	archive := tar.NewWriter(compressed)
	names := make([]string, 0, len(files))
	for name := range files {
		names = append(names, name)
	}
	for _, name := range names {
		body := files[name]
		header := &tar.Header{
			Name:     name,
			Mode:     0o644,
			Size:     int64(len(body)),
			ModTime:  time.Unix(0, 0),
			Typeflag: tar.TypeReg,
		}
		if err := archive.WriteHeader(header); err != nil {
			t.Fatalf("writing %s: %v", name, err)
		}
		if _, err := archive.Write([]byte(body)); err != nil {
			t.Fatalf("writing %s: %v", name, err)
		}
	}
	if err := archive.Close(); err != nil {
		t.Fatalf("closing the archive: %v", err)
	}
	if err := compressed.Close(); err != nil {
		t.Fatalf("closing the package: %v", err)
	}
	return buffer.Bytes()
}

// packEntriesAt is a package whose manifest is fine but which holds one more
// entry.
func packEntriesAt(t *testing.T, name, manifest string) []byte {
	t.Helper()

	return packEntries(t, map[string]string{
		"plugin.yaml": manifest,
		name:          "outside",
	})
}

// packSymlink is a package with a symbolic link in it.
func packSymlink(t *testing.T) []byte {
	t.Helper()

	var buffer bytes.Buffer
	compressed := gzip.NewWriter(&buffer)
	archive := tar.NewWriter(compressed)
	manifest := "id: example\nversion: 1.0.0\nname: E\n"
	if err := archive.WriteHeader(&tar.Header{
		Name: "plugin.yaml", Mode: 0o644, Size: int64(len(manifest)), Typeflag: tar.TypeReg,
		ModTime: time.Unix(0, 0),
	}); err != nil {
		t.Fatalf("writing the manifest: %v", err)
	}
	if _, err := archive.Write([]byte(manifest)); err != nil {
		t.Fatalf("writing the manifest: %v", err)
	}
	if err := archive.WriteHeader(&tar.Header{
		Name: "bin/service", Linkname: "/bin/sh", Typeflag: tar.TypeSymlink, Mode: 0o777,
		ModTime: time.Unix(0, 0),
	}); err != nil {
		t.Fatalf("writing the link: %v", err)
	}
	if err := archive.Close(); err != nil {
		t.Fatalf("closing the archive: %v", err)
	}
	if err := compressed.Close(); err != nil {
		t.Fatalf("closing the package: %v", err)
	}
	return buffer.Bytes()
}
