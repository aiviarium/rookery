// Package seal seals secrets with XChaCha20-Poly1305 under the deployment's key.
//
// A sealed value is the 24-byte nonce followed by the ciphertext and its tag.
// The secret row's id goes in as associated data, so a sealed value moved to
// another row does not open. Every row records the id of the key that sealed it,
// which is the key's own fingerprint, so a rekey reseals the rows one at a time
// under a new key while the old one still opens the rest.
package seal

import (
	"crypto/cipher"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"golang.org/x/crypto/chacha20poly1305"
)

// KeySize is the size of a key: 32 bytes, 64 hex digits.
const KeySize = chacha20poly1305.KeySize

// KeyIDLength is the length of a key id, in hex digits.
const KeyIDLength = 16

var (
	// ErrOpen reports a sealed value that does not open: another key, another
	// row, or a value that was changed.
	ErrOpen = errors.New("seal: the sealed value does not open")

	// ErrUnknownKey reports a value sealed with a key the ring does not hold.
	ErrUnknownKey = errors.New("seal: the key that sealed the value is not known")
)

// Key is one sealing key.
type Key struct {
	id   string
	aead cipher.AEAD
}

// New returns a random key.
func New() (*Key, error) {
	raw := make([]byte, KeySize)
	if _, err := rand.Read(raw); err != nil {
		return nil, fmt.Errorf("seal: reading a key: %w", err)
	}
	return NewKey(raw)
}

// NewKey builds a key from KeySize bytes.
func NewKey(raw []byte) (*Key, error) {
	if len(raw) != KeySize {
		return nil, fmt.Errorf("seal: a key is %d bytes, not %d", KeySize, len(raw))
	}
	aead, err := chacha20poly1305.NewX(raw)
	if err != nil {
		return nil, fmt.Errorf("seal: building the cipher: %w", err)
	}
	sum := sha256.Sum256(raw)
	return &Key{id: hex.EncodeToString(sum[:KeyIDLength/2]), aead: aead}, nil
}

// ParseKey decodes a key from 64 hex digits, which is how a key is written in
// ROOKERY_SECRET_KEY and in its file.
func ParseKey(text string) (*Key, error) {
	text = strings.TrimSpace(text)
	if len(text) != KeySize*2 {
		return nil, fmt.Errorf("seal: a key is %d hex digits, not %d", KeySize*2, len(text))
	}
	raw, err := hex.DecodeString(text)
	if err != nil {
		return nil, fmt.Errorf("seal: reading a key: %w", err)
	}
	return NewKey(raw)
}

// ID is the key's identifier: the first KeyIDLength hex digits of the SHA-256 of
// the key itself, so a key names itself and the id says nothing about the key.
func (k *Key) ID() string { return k.id }

// Seal encrypts plain, binding the result to associated — the row's id — and
// puts the random nonce in front of it.
func (k *Key) Seal(plain, associated []byte) ([]byte, error) {
	nonce := make([]byte, k.aead.NonceSize(), k.aead.NonceSize()+len(plain)+k.aead.Overhead())
	if _, err := rand.Read(nonce); err != nil {
		return nil, fmt.Errorf("seal: reading a nonce: %w", err)
	}
	return k.aead.Seal(nonce, nonce, plain, associated), nil
}

// Open decrypts a value sealed with this key and the same associated data.
func (k *Key) Open(sealed, associated []byte) ([]byte, error) {
	nonceSize := k.aead.NonceSize()
	if len(sealed) < nonceSize+k.aead.Overhead() {
		return nil, fmt.Errorf("%w: %d bytes is shorter than a sealed value", ErrOpen, len(sealed))
	}
	plain, err := k.aead.Open(nil, sealed[:nonceSize], sealed[nonceSize:], associated)
	if err != nil {
		return nil, fmt.Errorf("%w: %w", ErrOpen, err)
	}
	return plain, nil
}

// Ring holds the current key and the keys values are still sealed with, so that
// a deployment rekeys without a downtime.
type Ring struct {
	current *Key
	keys    map[string]*Key
}

// NewRing holds keys, one of them current, named by its id.
func NewRing(current string, keys ...*Key) (*Ring, error) {
	ring := &Ring{keys: make(map[string]*Key, len(keys))}
	for _, key := range keys {
		if key == nil {
			return nil, errors.New("seal: a ring cannot hold a nil key")
		}
		if _, seen := ring.keys[key.ID()]; seen {
			return nil, fmt.Errorf("seal: the key %s is in the ring twice", key.ID())
		}
		ring.keys[key.ID()] = key
	}
	ring.current = ring.keys[current]
	if ring.current == nil {
		return nil, fmt.Errorf("%w: %s", ErrUnknownKey, current)
	}
	return ring, nil
}

// Current is the key new values are sealed with.
func (r *Ring) Current() *Key { return r.current }

// Seal seals with the current key, and says which key it was.
func (r *Ring) Seal(plain, associated []byte) (sealed []byte, keyID string, err error) {
	sealed, err = r.current.Seal(plain, associated)
	if err != nil {
		return nil, "", err
	}
	return sealed, r.current.ID(), nil
}

// Open opens a value sealed with the key id.
func (r *Ring) Open(sealed []byte, keyID string, associated []byte) ([]byte, error) {
	key := r.keys[keyID]
	if key == nil {
		return nil, fmt.Errorf("%w: %s", ErrUnknownKey, keyID)
	}
	return key.Open(sealed, associated)
}

// Rekey reseals a value with the current key when another key sealed it, and
// says whether it changed anything. A value already under the current key is
// returned as it is.
func (r *Ring) Rekey(sealed []byte, keyID string, associated []byte) (out []byte, outKeyID string, changed bool, err error) {
	if keyID == r.current.ID() {
		return sealed, keyID, false, nil
	}
	plain, err := r.Open(sealed, keyID, associated)
	if err != nil {
		return nil, "", false, err
	}
	out, outKeyID, err = r.Seal(plain, associated)
	if err != nil {
		return nil, "", false, err
	}
	return out, outKeyID, true, nil
}

// Load returns the key in text when text is not empty, and otherwise the key in
// the file at path. With create, a missing file is filled with a new key of mode
// 0600: that is what the server does on an empty database, so that the key is
// there for the secrets that follow.
//
// text wins over path, so ROOKERY_SECRET_KEY overrides ROOKERY_SECRET_KEY_FILE.
func Load(text, path string, create bool) (*Key, error) {
	if text != "" {
		return ParseKey(text)
	}
	if path == "" {
		return nil, errors.New("seal: no key: neither ROOKERY_SECRET_KEY nor ROOKERY_SECRET_KEY_FILE is set")
	}

	key, err := readKeyFile(path)
	switch {
	case err == nil:
		return key, nil
	case !errors.Is(err, fs.ErrNotExist):
		return nil, err
	case !create:
		return nil, err
	}
	return createKeyFile(path)
}

// readKeyFile reads a key file written by createKeyFile or by an operator.
func readKeyFile(path string) (*Key, error) {
	// The path is the operator's ROOKERY_SECRET_KEY_FILE.
	text, err := os.ReadFile(path) //nolint:gosec
	if err != nil {
		return nil, fmt.Errorf("seal: reading the key file: %w", err)
	}
	key, err := ParseKey(string(text))
	if err != nil {
		return nil, fmt.Errorf("seal: the key file %s: %w", path, err)
	}
	return key, nil
}

// createKeyFile writes a new key to path, and reads the key another process
// wrote there first if it won the race.
func createKeyFile(path string) (*Key, error) {
	raw := make([]byte, KeySize)
	if _, err := rand.Read(raw); err != nil {
		return nil, fmt.Errorf("seal: reading a key: %w", err)
	}
	key, err := NewKey(raw)
	if err != nil {
		return nil, err
	}
	text := hex.EncodeToString(raw) + "\n"

	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return nil, fmt.Errorf("seal: making the key file's directory: %w", err)
	}

	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600) //nolint:gosec // the operator's path
	if errors.Is(err, fs.ErrExist) {
		// Another process made the key first: that one is the database's.
		return readKeyFile(path)
	}
	if err != nil {
		return nil, fmt.Errorf("seal: creating the key file: %w", err)
	}
	if _, err := file.WriteString(text); err != nil {
		_ = file.Close()
		return nil, fmt.Errorf("seal: writing the key file: %w", err)
	}
	if err := file.Close(); err != nil {
		return nil, fmt.Errorf("seal: writing the key file: %w", err)
	}
	return key, nil
}
