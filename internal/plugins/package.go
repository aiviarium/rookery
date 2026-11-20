package plugins

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path"
	"path/filepath"
	"strings"
	"time"
)

// Limits on a package, so a hostile or broken one cannot fill the disk.
const (
	// MaxPackageSize is the largest a package file may be.
	MaxPackageSize = 256 << 20
	// MaxFileSize is the largest one file in a package may be.
	MaxFileSize = 64 << 20
	// MaxFiles is how many files a package may hold.
	MaxFiles = 20000
)

// ErrPackage is the sentinel every refusal of a package wraps.
var ErrPackage = errors.New("plugins: invalid package")

// Packed is what Pack made.
type Packed struct {
	// Manifest is the package's plugin.yaml.
	Manifest *Manifest
	// SHA256 is the package's hash, in hex, which is its name in a registry.
	SHA256 string
	// Size is the package's size in bytes.
	Size int64
}

// Pack writes a .tar.gz of the plugin in dir to w, and returns its manifest and
// hash.
//
// The archive is reproducible: files in name order, no owner, a fixed
// modification time and modes from the executable bit, so packing the same plugin
// twice gives the same bytes and the same hash.
func Pack(ctx context.Context, w io.Writer, dir string) (Packed, error) {
	if err := ctx.Err(); err != nil {
		return Packed{}, fmt.Errorf("plugins: packing: %w", err)
	}
	manifest, err := Check(os.DirFS(dir))
	if err != nil {
		return Packed{}, err
	}

	hash := sha256.New()
	counted := &countingWriter{}
	compressed := gzip.NewWriter(io.MultiWriter(w, hash, counted))
	compressed.ModTime = time.Unix(0, 0)
	compressed.OS = 3 // Unix, whatever machine packed it.
	archive := tar.NewWriter(compressed)

	err = filepath.WalkDir(dir, func(name string, entry fs.DirEntry, err error) error {
		if err != nil {
			return fmt.Errorf("plugins: walking %s: %w", name, err)
		}
		if err := ctx.Err(); err != nil {
			return err
		}
		if name == dir {
			return nil
		}
		relative, err := filepath.Rel(dir, name)
		if err != nil {
			return fmt.Errorf("plugins: %s: %w", name, err)
		}
		relative = filepath.ToSlash(relative)
		// A repository's own bookkeeping is not part of a plugin.
		if relative == ".git" || strings.HasPrefix(relative, ".git/") {
			if entry.IsDir() {
				return filepath.SkipDir
			}
			return nil
		}
		return packEntry(archive, name, relative, entry)
	})
	if err != nil {
		_ = archive.Close()
		_ = compressed.Close()
		return Packed{}, fmt.Errorf("plugins: packing %s: %w", dir, err)
	}
	if err := archive.Close(); err != nil {
		return Packed{}, fmt.Errorf("plugins: closing the package: %w", err)
	}
	if err := compressed.Close(); err != nil {
		return Packed{}, fmt.Errorf("plugins: closing the package: %w", err)
	}
	return Packed{
		Manifest: manifest,
		SHA256:   hex.EncodeToString(hash.Sum(nil)),
		Size:     counted.n,
	}, nil
}

// packEntry writes one file or directory of a package.
func packEntry(archive *tar.Writer, name, relative string, entry fs.DirEntry) error {
	info, err := entry.Info()
	if err != nil {
		return fmt.Errorf("plugins: reading %s: %w", name, err)
	}
	header := &tar.Header{
		Name:     relative,
		ModTime:  time.Unix(0, 0),
		Uid:      0,
		Gid:      0,
		Uname:    "",
		Gname:    "",
		Format:   tar.FormatPAX,
		Typeflag: tar.TypeReg,
		Mode:     0o644,
	}
	if entry.IsDir() {
		header.Name += "/"
		header.Typeflag = tar.TypeDir
		header.Mode = 0o755
		if err := archive.WriteHeader(header); err != nil {
			return fmt.Errorf("plugins: writing %s: %w", relative, err)
		}
		return nil
	}
	if !info.Mode().IsRegular() {
		return fmt.Errorf("%w: %s is not a file", ErrPackage, relative)
	}
	if info.Size() > MaxFileSize {
		return fmt.Errorf("%w: %s is %d bytes", ErrPackage, relative, info.Size())
	}
	if info.Mode().Perm()&0o111 != 0 {
		header.Mode = 0o755
	}
	header.Size = info.Size()
	if err := archive.WriteHeader(header); err != nil {
		return fmt.Errorf("plugins: writing %s: %w", relative, err)
	}
	//nolint:gosec // the file is one the caller asked to pack.
	file, err := os.Open(name)
	if err != nil {
		return fmt.Errorf("plugins: reading %s: %w", name, err)
	}
	defer func() { _ = file.Close() }()

	written, err := io.Copy(archive, io.LimitReader(file, MaxFileSize))
	if err != nil {
		return fmt.Errorf("plugins: writing %s: %w", relative, err)
	}
	if written != info.Size() {
		return fmt.Errorf("%w: %s changed while it was packed", ErrPackage, relative)
	}
	return nil
}

// Unpacked is what a package held.
type Unpacked struct {
	// Manifest is the package's plugin.yaml.
	Manifest *Manifest
	// SHA256 is the package's hash, in hex.
	SHA256 string
	// Size is the package's size in bytes.
	Size    int64
	archive []byte
}

// Unpack reads a package from r and writes its files into dir, which it makes.
// The package's hash is the caller's to compare with what the registry or the
// installer expected.
//
// A package may hold files and directories only: a symbolic link, a device or a
// path that leaves dir is refused.
func Unpack(ctx context.Context, r io.Reader, dir string) (Unpacked, error) {
	if err := ctx.Err(); err != nil {
		return Unpacked{}, fmt.Errorf("plugins: unpacking: %w", err)
	}
	hash := sha256.New()
	counted := &countingWriter{}
	data, err := io.ReadAll(io.LimitReader(io.TeeReader(r, io.MultiWriter(hash, counted)), MaxPackageSize+1))
	if err != nil {
		return Unpacked{}, fmt.Errorf("plugins: reading the package: %w", err)
	}
	if int64(len(data)) > MaxPackageSize {
		return Unpacked{}, fmt.Errorf("%w: it is larger than %d bytes", ErrPackage, MaxPackageSize)
	}

	compressed, err := gzip.NewReader(bytes.NewReader(data))
	if err != nil {
		return Unpacked{}, fmt.Errorf("%w: it is not a gzip file: %w", ErrPackage, err)
	}
	defer func() { _ = compressed.Close() }()

	if err := os.MkdirAll(dir, 0o700); err != nil {
		return Unpacked{}, fmt.Errorf("plugins: making %s: %w", dir, err)
	}
	archive := tar.NewReader(compressed)
	files := 0
	var manifest *Manifest
	for {
		header, err := archive.Next()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return Unpacked{}, fmt.Errorf("%w: %w", ErrPackage, err)
		}
		name, err := packagePath(header.Name)
		if err != nil {
			return Unpacked{}, err
		}
		target := filepath.Join(dir, filepath.FromSlash(name))
		switch header.Typeflag {
		case tar.TypeDir:
			//nolint:gosec // the install directory is the server's own.
			if err := os.MkdirAll(target, 0o755); err != nil {
				return Unpacked{}, fmt.Errorf("plugins: making %s: %w", name, err)
			}
		case tar.TypeReg:
			files++
			if files > MaxFiles {
				return Unpacked{}, fmt.Errorf("%w: it has more than %d files", ErrPackage, MaxFiles)
			}
			if err := unpackFile(ctx, archive, dir, name, header); err != nil {
				return Unpacked{}, err
			}
			if name == FileName {
				manifest, err = loadFile(target)
				if err != nil {
					return Unpacked{}, err
				}
			}
		default:
			return Unpacked{}, fmt.Errorf("%w: %s is not a file or a directory", ErrPackage, name)
		}
	}
	if manifest == nil {
		return Unpacked{}, fmt.Errorf("%w: it has no %s", ErrPackage, FileName)
	}
	if _, err := Check(os.DirFS(dir)); err != nil {
		return Unpacked{}, err
	}
	return Unpacked{
		Manifest: manifest,
		SHA256:   hex.EncodeToString(hash.Sum(nil)),
		Size:     counted.n,
		archive:  data,
	}, nil
}

// unpackFile writes one file of a package.
func unpackFile(ctx context.Context, archive *tar.Reader, dir, name string, header *tar.Header) error {
	if header.Size > MaxFileSize {
		return fmt.Errorf("%w: %s is %d bytes", ErrPackage, name, header.Size)
	}
	target := filepath.Join(dir, filepath.FromSlash(name))
	//nolint:gosec // the install directory is the server's own.
	if err := os.MkdirAll(filepath.Dir(target), 0o755); err != nil {
		return fmt.Errorf("plugins: making the directory of %s: %w", name, err)
	}
	mode := os.FileMode(0o644)
	if header.Mode&0o111 != 0 {
		mode = 0o755
	}
	//nolint:gosec // the mode comes from the archive, and is one of 0644 and 0755.
	file, err := os.OpenFile(target, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, mode)
	if err != nil {
		return fmt.Errorf("plugins: writing %s: %w", name, err)
	}
	defer func() { _ = file.Close() }()

	written, err := io.Copy(file, io.LimitReader(archive, MaxFileSize))
	if err != nil {
		return fmt.Errorf("plugins: writing %s: %w", name, err)
	}
	if written != header.Size {
		return fmt.Errorf("%w: %s is shorter than its header says", ErrPackage, name)
	}
	if err := ctx.Err(); err != nil {
		return fmt.Errorf("plugins: unpacking: %w", err)
	}
	return nil
}

// packagePath checks an archive entry's name: relative, inside the package, and
// with no empty parts.
func packagePath(name string) (string, error) {
	clean := path.Clean(strings.TrimSuffix(name, "/"))
	switch {
	case clean == "." || clean == "":
		return "", fmt.Errorf("%w: an entry has no name", ErrPackage)
	case strings.HasPrefix(clean, "/"):
		return "", fmt.Errorf("%w: %s is an absolute path", ErrPackage, name)
	case clean == ".." || strings.HasPrefix(clean, "../"):
		return "", fmt.Errorf("%w: %s leaves the package", ErrPackage, name)
	}
	return clean, nil
}

// ManifestOf reads a package's manifest without writing anything: what a
// registry's index needs.
func ManifestOf(r io.Reader) (*Manifest, error) {
	compressed, err := gzip.NewReader(r)
	if err != nil {
		return nil, fmt.Errorf("%w: it is not a gzip file: %w", ErrPackage, err)
	}
	defer func() { _ = compressed.Close() }()

	archive := tar.NewReader(compressed)
	for {
		header, err := archive.Next()
		if errors.Is(err, io.EOF) {
			return nil, fmt.Errorf("%w: it has no %s", ErrPackage, FileName)
		}
		if err != nil {
			return nil, fmt.Errorf("%w: %w", ErrPackage, err)
		}
		if header.Typeflag != tar.TypeReg || path.Clean(header.Name) != FileName {
			continue
		}
		manifest, err := parseManifest(io.LimitReader(archive, MaxFileSize))
		if err != nil {
			return nil, err
		}
		return manifest, nil
	}
}

// countingWriter counts what was written through it.
type countingWriter struct{ n int64 }

func (w *countingWriter) Write(p []byte) (int, error) {
	w.n += int64(len(p))
	return len(p), nil
}
