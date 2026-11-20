package seal_test

import (
	"bytes"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/aiviarium/rookery/internal/seal"
)

func TestSealAndOpen(t *testing.T) {
	t.Parallel()

	key, err := seal.New()
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	cases := []struct {
		name  string
		plain []byte
		rowID []byte
	}{
		{name: "empty", plain: nil, rowID: []byte("01JC0M0M")},
		{name: "small", plain: []byte("hunter2"), rowID: []byte("01JC0M0M")},
		{name: "binary", plain: []byte{0, 1, 2, 0xff, 0x00}, rowID: []byte("row")},
		{name: "large", plain: bytes.Repeat([]byte("a secret "), 1<<17), rowID: []byte("row")},
		{name: "no associated data", plain: []byte("x"), rowID: nil},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			sealed, err := key.Seal(tc.plain, tc.rowID)
			if err != nil {
				t.Fatalf("Seal: %v", err)
			}
			if bytes.Contains(sealed, tc.plain) && len(tc.plain) > 4 {
				t.Error("the sealed value contains the plaintext")
			}

			opened, err := key.Open(sealed, tc.rowID)
			if err != nil {
				t.Fatalf("Open: %v", err)
			}
			if !bytes.Equal(opened, tc.plain) {
				t.Errorf("Open: got %q, want %q", opened, tc.plain)
			}
		})
	}
}

func TestOpenRefuses(t *testing.T) {
	t.Parallel()

	key, err := seal.New()
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	other, err := seal.New()
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	sealed, err := key.Seal([]byte("hunter2"), []byte("row-1"))
	if err != nil {
		t.Fatalf("Seal: %v", err)
	}

	flippedNonce := bytes.Clone(sealed)
	flippedNonce[0] ^= 0xff
	flippedTag := bytes.Clone(sealed)
	flippedTag[len(flippedTag)-1] ^= 0xff

	cases := []struct {
		name string
		open func() error
	}{
		{name: "another row's id", open: func() error {
			_, err := key.Open(sealed, []byte("row-2"))
			return err
		}},
		{name: "the row's id is missing", open: func() error {
			_, err := key.Open(sealed, nil)
			return err
		}},
		{name: "another key", open: func() error {
			_, err := other.Open(sealed, []byte("row-1"))
			return err
		}},
		{name: "the nonce was changed", open: func() error {
			_, err := key.Open(flippedNonce, []byte("row-1"))
			return err
		}},
		{name: "the tag was changed", open: func() error {
			_, err := key.Open(flippedTag, []byte("row-1"))
			return err
		}},
		{name: "the value was truncated", open: func() error {
			_, err := key.Open(sealed[:len(sealed)-1], []byte("row-1"))
			return err
		}},
		{name: "empty", open: func() error {
			_, err := key.Open(nil, []byte("row-1"))
			return err
		}},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			if err := tc.open(); !errors.Is(err, seal.ErrOpen) {
				t.Errorf("got %v, want seal.ErrOpen", err)
			}
		})
	}
}

func TestNoncesDiffer(t *testing.T) {
	t.Parallel()

	key, err := seal.New()
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	first, err := key.Seal([]byte("same"), []byte("row"))
	if err != nil {
		t.Fatalf("Seal: %v", err)
	}
	second, err := key.Seal([]byte("same"), []byte("row"))
	if err != nil {
		t.Fatalf("Seal: %v", err)
	}
	if bytes.Equal(first, second) {
		t.Error("two seals of one value are the same bytes")
	}
}

func TestParseKey(t *testing.T) {
	t.Parallel()

	raw := bytes.Repeat([]byte{0x01}, seal.KeySize)
	text := strings.Repeat("01", seal.KeySize)

	cases := []struct {
		name    string
		text    string
		wantErr bool
	}{
		{name: "hex", text: text},
		{name: "hex with a newline", text: text + "\n"},
		{name: "uppercase", text: strings.Repeat("AB", seal.KeySize)},
		{name: "too short", text: strings.Repeat("01", seal.KeySize-1), wantErr: true},
		{name: "too long", text: strings.Repeat("01", seal.KeySize+1), wantErr: true},
		{name: "not hex", text: strings.Repeat("zz", seal.KeySize), wantErr: true},
		{name: "empty", text: "", wantErr: true},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			key, err := seal.ParseKey(tc.text)
			if tc.wantErr {
				if err == nil {
					t.Fatalf("ParseKey accepted %q", tc.text)
				}
				return
			}
			if err != nil {
				t.Fatalf("ParseKey: %v", err)
			}
			if key.ID() == "" {
				t.Error("the key has no id")
			}
		})
	}

	key, err := seal.ParseKey(text)
	if err != nil {
		t.Fatalf("ParseKey: %v", err)
	}
	again, err := seal.ParseKey(strings.ToUpper(text))
	if err != nil {
		t.Fatalf("ParseKey uppercase: %v", err)
	}
	if key.ID() != again.ID() {
		t.Errorf("the same key in two cases has two ids: %s, %s", key.ID(), again.ID())
	}
	same, err := seal.NewKey(raw)
	if err != nil {
		t.Fatalf("NewKey: %v", err)
	}
	if key.ID() != same.ID() {
		t.Errorf("NewKey and ParseKey disagree: %s, %s", key.ID(), same.ID())
	}
	if len(key.ID()) != seal.KeyIDLength {
		t.Errorf("the id is %d characters, want %d", len(key.ID()), seal.KeyIDLength)
	}
}

func TestNewKeyRefuses(t *testing.T) {
	t.Parallel()

	if _, err := seal.NewKey(make([]byte, seal.KeySize-1)); err == nil {
		t.Error("NewKey accepted a short key")
	}
	if _, err := seal.NewKey(make([]byte, seal.KeySize+1)); err == nil {
		t.Error("NewKey accepted a long key")
	}
}

func TestNewKeysDiffer(t *testing.T) {
	t.Parallel()

	seen := make(map[string]bool)
	for range 16 {
		key, err := seal.New()
		if err != nil {
			t.Fatalf("New: %v", err)
		}
		if seen[key.ID()] {
			t.Fatalf("New made the key %s twice", key.ID())
		}
		seen[key.ID()] = true
	}
}

func TestLoad(t *testing.T) {
	t.Parallel()

	key, err := seal.New()
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	t.Run("from text", func(t *testing.T) {
		t.Parallel()

		loaded, err := seal.Load(strings.Repeat("02", seal.KeySize), "", false)
		if err != nil {
			t.Fatalf("Load: %v", err)
		}
		if loaded.ID() == "" {
			t.Error("the key has no id")
		}
	})

	t.Run("text wins over a file", func(t *testing.T) {
		t.Parallel()

		path := filepath.Join(t.TempDir(), "secret.key")
		if err := os.WriteFile(path, []byte(strings.Repeat("03", seal.KeySize)), 0o600); err != nil {
			t.Fatalf("WriteFile: %v", err)
		}

		fromText, err := seal.Load(strings.Repeat("02", seal.KeySize), path, false)
		if err != nil {
			t.Fatalf("Load: %v", err)
		}
		fromFile, err := seal.Load("", path, false)
		if err != nil {
			t.Fatalf("Load: %v", err)
		}
		if fromText.ID() == fromFile.ID() {
			t.Error("Load used the file although a key was given")
		}
	})

	t.Run("a missing file without create", func(t *testing.T) {
		t.Parallel()

		_, err := seal.Load("", filepath.Join(t.TempDir(), "secret.key"), false)
		if err == nil {
			t.Fatal("Load made a key although create is false")
		}
	})

	t.Run("neither text nor path", func(t *testing.T) {
		t.Parallel()

		_, err := seal.Load("", "", true)
		if err == nil {
			t.Fatal("Load returned a key with nothing to load")
		}
	})

	t.Run("created then read", func(t *testing.T) {
		t.Parallel()

		path := filepath.Join(t.TempDir(), "state", "secret.key")
		created, err := seal.Load("", path, true)
		if err != nil {
			t.Fatalf("Load: %v", err)
		}

		info, err := os.Stat(path)
		if err != nil {
			t.Fatalf("Stat: %v", err)
		}
		if mode := info.Mode().Perm(); mode != 0o600 {
			t.Errorf("the key file's mode is %o, want 600", mode)
		}

		again, err := seal.Load("", path, true)
		if err != nil {
			t.Fatalf("Load again: %v", err)
		}
		if again.ID() != created.ID() {
			t.Errorf("the second Load read another key: %s, %s", again.ID(), created.ID())
		}
		if key.ID() == created.ID() {
			t.Error("a created key repeats a key made earlier")
		}
	})
}

func TestRing(t *testing.T) {
	t.Parallel()

	old, err := seal.New()
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	current, err := seal.New()
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	ring, err := seal.NewRing(current.ID(), old, current)
	if err != nil {
		t.Fatalf("NewRing: %v", err)
	}
	if ring.Current().ID() != current.ID() {
		t.Errorf("Current: got %s, want %s", ring.Current().ID(), current.ID())
	}

	rowID := []byte("row-1")
	oldSealed, _, err := func() ([]byte, string, error) {
		sealed, err := old.Seal([]byte("hunter2"), rowID)
		return sealed, old.ID(), err
	}()
	if err != nil {
		t.Fatalf("Seal: %v", err)
	}

	// The ring opens what the old key sealed.
	opened, err := ring.Open(oldSealed, old.ID(), rowID)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	if string(opened) != "hunter2" {
		t.Errorf("Open: got %q, want %q", opened, "hunter2")
	}

	// And reseals it under the current key.
	rekeyed, rekeyedID, changed, err := ring.Rekey(oldSealed, old.ID(), rowID)
	if err != nil {
		t.Fatalf("Rekey: %v", err)
	}
	if !changed {
		t.Error("Rekey left a value under the old key")
	}
	if rekeyedID != current.ID() {
		t.Errorf("Rekey: got key %s, want %s", rekeyedID, current.ID())
	}
	if bytes.Equal(rekeyed, oldSealed) {
		t.Error("Rekey returned the old value")
	}
	opened, err = ring.Open(rekeyed, rekeyedID, rowID)
	if err != nil {
		t.Fatalf("Open after Rekey: %v", err)
	}
	if string(opened) != "hunter2" {
		t.Errorf("Open after Rekey: got %q, want %q", opened, "hunter2")
	}

	// A value already current is left alone.
	same, sameID, changed, err := ring.Rekey(rekeyed, rekeyedID, rowID)
	if err != nil {
		t.Fatalf("Rekey: %v", err)
	}
	if changed {
		t.Error("Rekey changed a value that was already current")
	}
	if !bytes.Equal(same, rekeyed) || sameID != rekeyedID {
		t.Error("Rekey returned another value")
	}

	// An unknown key id is an error, not a silent pass.
	if _, err := ring.Open(rekeyed, "0000000000000000", rowID); !errors.Is(err, seal.ErrUnknownKey) {
		t.Errorf("Open with an unknown key: got %v, want seal.ErrUnknownKey", err)
	}
	if _, _, _, err := ring.Rekey(rekeyed, "0000000000000000", rowID); !errors.Is(err, seal.ErrUnknownKey) {
		t.Errorf("Rekey with an unknown key: got %v, want seal.ErrUnknownKey", err)
	}
}

func TestNewRingRefuses(t *testing.T) {
	t.Parallel()

	key, err := seal.New()
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	if _, err := seal.NewRing("0000000000000000", key); !errors.Is(err, seal.ErrUnknownKey) {
		t.Errorf("NewRing without the current key: got %v, want seal.ErrUnknownKey", err)
	}
	if _, err := seal.NewRing(key.ID()); !errors.Is(err, seal.ErrUnknownKey) {
		t.Errorf("NewRing with no keys: got %v, want seal.ErrUnknownKey", err)
	}
	if _, err := seal.NewRing(key.ID(), key, key); err == nil {
		t.Error("NewRing accepted the same key twice")
	}
	if _, err := seal.NewRing(key.ID(), nil); err == nil {
		t.Error("NewRing accepted a nil key")
	}
}
