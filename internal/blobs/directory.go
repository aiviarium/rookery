package blobs

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"hash"
	"io"
	"os"
	"path/filepath"
	"strings"
)

// Directory is a backend that keeps blobs in a directory, under two levels
// named for the first four hex digits of their SHA-256.
type Directory struct {
	root string
}

// NewDirectory returns a directory backend.
func NewDirectory(root string) (*Directory, error) {
	if root == "" {
		return nil, errors.New("blobs: a directory backend needs a directory")
	}
	if err := os.MkdirAll(root, 0o750); err != nil {
		return nil, fmt.Errorf("blobs: making %s: %w", root, err)
	}
	return &Directory{root: root}, nil
}

// Put stores a stream and returns its SHA-256 and its size.
func (d *Directory) Put(ctx context.Context, body io.Reader) (string, int64, error) {
	if err := os.MkdirAll(filepath.Join(d.root, "tmp"), 0o750); err != nil {
		return "", 0, fmt.Errorf("blobs: making the directory's temporary space: %w", err)
	}
	temporary, err := os.CreateTemp(filepath.Join(d.root, "tmp"), "blob-")
	if err != nil {
		return "", 0, fmt.Errorf("blobs: making a temporary file: %w", err)
	}
	temporaryPath := temporary.Name()
	defer func() {
		_ = temporary.Close()
		_ = os.Remove(temporaryPath)
	}()

	id, size, err := checksumAndCopy(ctx, temporary, body)
	if err != nil {
		return "", 0, err
	}
	if err := temporary.Close(); err != nil {
		return "", 0, fmt.Errorf("blobs: writing %s: %w", temporaryPath, err)
	}

	destination := d.path(id)
	if err := os.MkdirAll(filepath.Dir(destination), 0o750); err != nil {
		return "", 0, fmt.Errorf("blobs: making %s: %w", filepath.Dir(destination), err)
	}
	if _, err := os.Stat(destination); err == nil {
		// The same bytes are already there: a blob is its content.
		return id, size, nil
	}
	if err := os.Rename(temporaryPath, destination); err != nil {
		return "", 0, fmt.Errorf("blobs: storing %s: %w", id, err)
	}
	return id, size, nil
}

// Open returns a blob's bytes.
func (d *Directory) Open(_ context.Context, id string) (io.ReadSeekCloser, int64, error) {
	file, err := os.Open(d.path(id)) //nolint:gosec // the id is a SHA-256 this package made
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, 0, fmt.Errorf("%w: the bytes of %s are not there", errors.New("blobs: missing"), id)
		}
		return nil, 0, fmt.Errorf("blobs: opening %s: %w", id, err)
	}
	info, err := file.Stat()
	if err != nil {
		_ = file.Close()
		return nil, 0, fmt.Errorf("blobs: reading %s: %w", id, err)
	}
	return file, info.Size(), nil
}

// Delete removes a blob's bytes.
func (d *Directory) Delete(_ context.Context, id string) error {
	if err := os.Remove(d.path(id)); err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("blobs: deleting %s: %w", id, err)
	}
	return nil
}

// path is where a blob's bytes are.
func (d *Directory) path(id string) string {
	return filepath.Join(d.root, id[:2], id)
}

// sha256Hash is the hash a blob is named by.
func sha256Hash() hash.Hash { return sha256.New() }

// hexOf is a hash in hex.
func hexOf(sum []byte) string { return hex.EncodeToString(sum) }

// checksumAndCopy writes body to out while hashing it.
func checksumAndCopy(ctx context.Context, out io.Writer, body io.Reader) (string, int64, error) {
	hasher := sha256Hash()
	buffer := make([]byte, 64<<10)
	var size int64
	for {
		if err := ctx.Err(); err != nil {
			return "", 0, err
		}
		read, err := body.Read(buffer)
		if read > 0 {
			size += int64(read)
			if _, writeErr := hasher.Write(buffer[:read]); writeErr != nil {
				return "", 0, fmt.Errorf("blobs: hashing: %w", writeErr)
			}
			if _, writeErr := out.Write(buffer[:read]); writeErr != nil {
				return "", 0, fmt.Errorf("blobs: writing: %w", writeErr)
			}
		}
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return "", 0, fmt.Errorf("blobs: reading: %w", err)
		}
	}
	return hexOf(hasher.Sum(nil)), size, nil
}

// NewLocation returns the backend a location names: a directory, or
// s3://bucket/prefix.
func NewLocation(location string, s3 S3Config) (Backend, error) {
	if strings.HasPrefix(location, "s3://") {
		return NewS3(location, s3)
	}
	return NewDirectory(location)
}
