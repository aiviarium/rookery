package plugins

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net/url"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

// IndexFormat is the index format's version: a registry's index.json says which
// one it is written in.
const IndexFormat = 1

// ErrSignature is what an index that is not signed by the registry's key answers
// with.
var ErrSignature = errors.New("plugins: the index's signature is not the registry's")

// Index is a registry's index.json: the static HTTPS directory's list of
// plugins, their versions and where their packages are.
type Index struct {
	// Version is the format's version, IndexFormat.
	Version int `json:"version"`
	// Plugins are the plugins the registry has, by id.
	Plugins []IndexPlugin `json:"plugins"`
	// Generated is when the index was written, in RFC 3339.
	Generated string `json:"generated,omitempty"`
}

// IndexPlugin is one plugin in an index.
type IndexPlugin struct {
	ID string `json:"id"`
	// Name is what people see, from the newest package's manifest.
	Name string `json:"name,omitempty"`
	// Versions are the packages the registry holds, oldest first.
	Versions []IndexVersion `json:"versions"`
}

// IndexVersion is one package in a registry.
type IndexVersion struct {
	Version string `json:"version"`
	// URL is where the package is, relative to the index or absolute.
	URL string `json:"url"`
	// SHA256 is the package's hash, in hex, which the installer checks.
	SHA256 string `json:"sha256"`
	// Size is the package's size in bytes.
	Size int64 `json:"size"`
	// Requires is the range of Rookery versions the package needs, as its
	// manifest says.
	Requires string `json:"requires,omitempty"`
}

// EncodeIndex writes an index as the bytes a signature is made over.
func EncodeIndex(index Index) ([]byte, error) {
	encoded, err := json.MarshalIndent(index, "", "  ")
	if err != nil {
		return nil, fmt.Errorf("plugins: writing the index: %w", err)
	}
	return append(encoded, '\n'), nil
}

// DecodeIndex reads an index. A field this build does not know is ignored, so a
// newer registry's index still works with an older Rookery.
func DecodeIndex(data []byte) (Index, error) {
	var index Index
	if err := json.Unmarshal(data, &index); err != nil {
		return Index{}, fmt.Errorf("%w: reading the index: %w", ErrInvalid, err)
	}
	switch index.Version {
	case 0, IndexFormat:
		index.Version = IndexFormat
	default:
		return Index{}, fmt.Errorf("%w: the index is version %d and this Rookery reads %d", ErrInvalid, index.Version, IndexFormat)
	}
	seen := make(map[string]bool, len(index.Plugins))
	for _, plugin := range index.Plugins {
		if !pluginID.MatchString(plugin.ID) {
			return Index{}, invalid("the index has plugin %q, which is not an id", plugin.ID)
		}
		if seen[plugin.ID] {
			return Index{}, invalid("the index lists plugin %s twice", plugin.ID)
		}
		seen[plugin.ID] = true
		versions := make(map[string]bool, len(plugin.Versions))
		for _, version := range plugin.Versions {
			parsed, err := releaseVersion(version.Version)
			if err != nil {
				return Index{}, fmt.Errorf("index: plugin %s: %w", plugin.ID, err)
			}
			if versions[parsed.String()] {
				return Index{}, invalid("the index lists %s@%s twice", plugin.ID, version.Version)
			}
			versions[parsed.String()] = true
			if _, err := ParseRequirement(version.Requires); err != nil {
				return Index{}, err
			}
			if version.URL == "" {
				return Index{}, invalid("the index has no URL for %s@%s", plugin.ID, version.Version)
			}
			if len(version.SHA256) != sha256.Size*2 {
				return Index{}, invalid("the index has no SHA-256 for %s@%s", plugin.ID, version.Version)
			}
			if _, err := hex.DecodeString(version.SHA256); err != nil {
				return Index{}, invalid("the index's SHA-256 for %s@%s is not hex", plugin.ID, version.Version)
			}
		}
	}
	return index, nil
}

// Sign signs an index with a registry's key. The signature goes beside the index
// as index.json.sig.
func Sign(key ed25519.PrivateKey, index []byte) ([]byte, error) {
	if len(key) != ed25519.PrivateKeySize {
		return nil, fmt.Errorf("%w: the registry's key is not an Ed25519 private key", ErrSignature)
	}
	return ed25519.Sign(key, index), nil
}

// Verify checks that an index is the one a registry signed.
func Verify(key ed25519.PublicKey, index, signature []byte) error {
	if len(key) != ed25519.PublicKeySize {
		return fmt.Errorf("%w: the registry's key is not an Ed25519 public key", ErrSignature)
	}
	if !ed25519.Verify(key, index, signature) {
		return fmt.Errorf("%w: the index is not the one it signed, or the wrong registry's key is configured", ErrSignature)
	}
	return nil
}

// LoadIndex reads an index and checks its signature: what installing from a
// registry starts with.
func LoadIndex(index, signature []byte, key ed25519.PublicKey) (Index, error) {
	if err := Verify(key, index, signature); err != nil {
		return Index{}, err
	}
	return DecodeIndex(index)
}

// Resolve finds a package in the index by "id@version", or by "id" alone, which
// is the newest version the registry holds.
func (i Index) Resolve(reference string) (IndexVersion, error) {
	id, wanted, hasVersion := strings.Cut(reference, "@")
	if !pluginID.MatchString(id) {
		return IndexVersion{}, invalid("%q is not a plugin id", id)
	}
	var plugin *IndexPlugin
	for index := range i.Plugins {
		if i.Plugins[index].ID == id {
			plugin = &i.Plugins[index]
			break
		}
	}
	if plugin == nil {
		return IndexVersion{}, fmt.Errorf("%w: the registry has no plugin %s", ErrNotFound, id)
	}
	if !hasVersion || wanted == "" {
		if len(plugin.Versions) == 0 {
			return IndexVersion{}, fmt.Errorf("%w: the registry has no version of %s", ErrNotFound, id)
		}
		newest := plugin.Versions[0]
		for _, version := range plugin.Versions[1:] {
			if compareVersionsOrZero(version.Version, newest.Version) > 0 {
				newest = version
			}
		}
		return newest, nil
	}

	want, err := ParseVersion(wanted)
	if err != nil {
		return IndexVersion{}, err
	}
	for _, version := range plugin.Versions {
		if candidate, err := releaseVersion(version.Version); err == nil && candidate == want {
			return version, nil
		}
	}
	return IndexVersion{}, fmt.Errorf("%w: the registry has no %s@%s", ErrNotFound, id, wanted)
}

// ErrNotFound is what a registry answers when it does not have a plugin.
var ErrNotFound = errors.New("plugins: not found")

// compareVersionsOrZero compares two versions, and puts one it cannot read first.
func compareVersionsOrZero(a, b string) int {
	left, err := ParseVersion(a)
	if err != nil {
		return -1
	}
	right, err := ParseVersion(b)
	if err != nil {
		return 1
	}
	return left.Compare(right)
}

// IndexDir writes and signs an index of the packages in dir: every .tar.gz below
// it, with its path relative to dir as its URL.
//
// It returns the index and its signature, which are written to index.json and
// index.json.sig. The same directory always gives the same index.
func IndexDir(ctx context.Context, dir string, key ed25519.PrivateKey) ([]byte, []byte, error) {
	if len(key) != ed25519.PrivateKeySize {
		return nil, nil, fmt.Errorf("%w: the registry's key is not an Ed25519 private key", ErrSignature)
	}
	var packages []string
	err := filepath.WalkDir(dir, func(name string, entry fs.DirEntry, err error) error {
		if err != nil {
			return fmt.Errorf("plugins: walking %s: %w", name, err)
		}
		if entry.IsDir() || !strings.HasSuffix(name, ".tar.gz") {
			return nil
		}
		packages = append(packages, name)
		return nil
	})
	if err != nil {
		return nil, nil, err
	}
	sort.Strings(packages)

	index := Index{Version: IndexFormat, Plugins: []IndexPlugin{}}
	for _, name := range packages {
		if err := ctx.Err(); err != nil {
			return nil, nil, fmt.Errorf("plugins: indexing %s: %w", dir, err)
		}
		version, manifest, err := indexPackage(dir, name)
		if err != nil {
			return nil, nil, err
		}
		if _, err := index.Resolve(manifest.ID + "@" + version.Version); err == nil {
			return nil, nil, fmt.Errorf("%w: two packages of %s@%s", ErrInvalid, manifest.ID, version.Version)
		}
		at := -1
		for i := range index.Plugins {
			if index.Plugins[i].ID == manifest.ID {
				at = i
				break
			}
		}
		if at < 0 {
			index.Plugins = append(index.Plugins, IndexPlugin{ID: manifest.ID, Name: manifest.Name})
			at = len(index.Plugins) - 1
		}
		index.Plugins[at].Versions = append(index.Plugins[at].Versions, version)
	}
	sort.Slice(index.Plugins, func(a, b int) bool { return index.Plugins[a].ID < index.Plugins[b].ID })
	for i := range index.Plugins {
		versions := index.Plugins[i].Versions
		sort.SliceStable(versions, func(a, b int) bool {
			return compareVersionsOrZero(versions[a].Version, versions[b].Version) < 0
		})
	}

	encoded, err := EncodeIndex(index)
	if err != nil {
		return nil, nil, err
	}
	signature, err := Sign(key, encoded)
	if err != nil {
		return nil, nil, err
	}
	return encoded, signature, nil
}

// indexPackage reads one package file: its version, its manifest, its hash and
// its path in the registry.
func indexPackage(dir, name string) (IndexVersion, *Manifest, error) {
	//nolint:gosec // the package is one the registry directory holds.
	file, err := os.Open(name)
	if err != nil {
		return IndexVersion{}, nil, fmt.Errorf("plugins: reading %s: %w", name, err)
	}
	defer func() { _ = file.Close() }()

	hash := sha256.New()
	counted := &countingWriter{}
	data, err := io.ReadAll(io.LimitReader(io.TeeReader(file, io.MultiWriter(hash, counted)), MaxPackageSize+1))
	if err != nil {
		return IndexVersion{}, nil, fmt.Errorf("plugins: reading %s: %w", name, err)
	}
	if int64(len(data)) > MaxPackageSize {
		return IndexVersion{}, nil, fmt.Errorf("%w: %s is larger than %d bytes", ErrPackage, name, MaxPackageSize)
	}
	manifest, err := ManifestOf(bytes.NewReader(data))
	if err != nil {
		return IndexVersion{}, nil, fmt.Errorf("plugins: %s: %w", name, err)
	}
	relative, err := filepath.Rel(dir, name)
	if err != nil {
		return IndexVersion{}, nil, fmt.Errorf("plugins: %s: %w", name, err)
	}
	return IndexVersion{
		Version:  manifest.Version,
		URL:      (&url.URL{Path: filepath.ToSlash(relative)}).String(),
		SHA256:   hex.EncodeToString(hash.Sum(nil)),
		Size:     counted.n,
		Requires: manifest.Requires,
	}, manifest, nil
}
