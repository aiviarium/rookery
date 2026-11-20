package link

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/pem"
	"errors"
	"fmt"
	"sync"

	"github.com/jackc/pgx/v5"
	"golang.org/x/crypto/ssh"

	"github.com/aiviarium/rookery/internal/id"
	"github.com/aiviarium/rookery/internal/seal"
	"github.com/aiviarium/rookery/internal/store"
)

// Key is one key of the ring: its record, and the signer links use.
type Key struct {
	// ID, Name and Fingerprint are the row's.
	ID          string
	Name        string
	Fingerprint string
	// Public is the key's authorized_keys line, without a comment.
	Public string
	// Record is the row as it is stored.
	Record store.RookeryKeys

	signer ssh.Signer
}

// Signer is the ssh.Signer links authenticate with.
func (k *Key) Signer() ssh.Signer { return k.signer }

// Ring is the SSH key ring: the keys the server logs in to its hosts with. One
// key is active; a rotation makes a new one active and retires the old one,
// which stays until no host's authorized_keys names it any more.
//
// The private halves are sealed with the deployment's key and the row's id as
// associated data, so a key row moved to another id does not open.
type Ring struct {
	store  *store.Store
	sealed *seal.Ring

	mu     sync.Mutex
	active *Key
}

// NewRing returns the key ring for a database. It does not read or make a key
// until the first link needs one.
func NewRing(opened *store.Store, sealed *seal.Ring) *Ring {
	return &Ring{store: opened, sealed: sealed}
}

// Active returns the key links log in with, making one on a database that has
// none.
func (r *Ring) Active(ctx context.Context) (*Key, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.active != nil {
		return r.active, nil
	}

	row, err := r.store.Queries().GetActiveSSHKey(ctx)
	switch {
	case err == nil:
		key, err := r.open(row)
		if err != nil {
			return nil, err
		}
		r.active = key
		return key, nil
	case !errors.Is(err, pgx.ErrNoRows):
		return nil, fmt.Errorf("link: reading the active key: %w", err)
	}

	key, _, err := r.create(ctx, "default")
	if err != nil {
		return nil, err
	}
	r.active = key
	return key, nil
}

// Rotate makes a new key active and retires the one it replaces.
func (r *Ring) Rotate(ctx context.Context, name string) (*Key, *Key, error) {
	r.mu.Lock()
	defer r.mu.Unlock()

	if name == "" {
		name = "default"
	}
	previous, err := r.store.Queries().GetActiveSSHKey(ctx)
	if err != nil && !errors.Is(err, pgx.ErrNoRows) {
		return nil, nil, fmt.Errorf("link: reading the active key: %w", err)
	}

	key, _, err := r.create(ctx, name)
	if err != nil {
		return nil, nil, err
	}
	var old *Key
	if err == nil && previous.ID != "" {
		if err := r.store.Queries().RetireSSHKey(ctx, previous.ID); err != nil {
			return nil, nil, fmt.Errorf("link: retiring the key %s: %w", previous.ID, err)
		}
		old, err = r.open(previous)
		if err != nil {
			return nil, nil, err
		}
	}
	r.active = key
	return key, old, nil
}

// List returns every key of the ring, newest first.
func (r *Ring) List(ctx context.Context) ([]Key, error) {
	rows, err := r.store.Queries().ListSSHKeys(ctx)
	if err != nil {
		return nil, fmt.Errorf("link: listing the keys: %w", err)
	}
	keys := make([]Key, 0, len(rows))
	for _, row := range rows {
		key, err := r.open(row)
		if err != nil {
			return nil, err
		}
		keys = append(keys, *key)
	}
	return keys, nil
}

// Signer returns the signer of a key by its record, which is how an apply gets
// the active key's private half without a database round trip.
func (r *Ring) Signer(ctx context.Context) (ssh.Signer, error) {
	key, err := r.Active(ctx)
	if err != nil {
		return nil, err
	}
	return key.signer, nil
}

// PrivatePEM opens the private key selected for an apply.
func (r *Ring) PrivatePEM(key *Key) ([]byte, error) {
	private, err := r.sealed.Open(key.Record.PrivateKey, key.Record.KeyID, []byte(key.ID))
	if err != nil {
		return nil, fmt.Errorf("link: opening the private key for apply: %w", err)
	}
	return private, nil
}

// create makes and stores a key.
func (r *Ring) create(ctx context.Context, name string) (*Key, string, error) {
	public, private, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		return nil, "", fmt.Errorf("link: generating a key: %w", err)
	}
	block, err := ssh.MarshalPrivateKey(private, "")
	if err != nil {
		return nil, "", fmt.Errorf("link: encoding a key: %w", err)
	}
	privatePEM := pem.EncodeToMemory(block)

	sshPublic, err := ssh.NewPublicKey(public)
	if err != nil {
		return nil, "", fmt.Errorf("link: building the public key: %w", err)
	}
	keyID := id.New()
	sealed, sealingKeyID, err := r.sealed.Seal(privatePEM, []byte(keyID))
	if err != nil {
		return nil, "", fmt.Errorf("link: sealing a key: %w", err)
	}

	row, err := r.store.Queries().CreateSSHKey(ctx, store.CreateSSHKeyParams{
		ID:          keyID,
		Name:        name,
		KeyType:     sshPublic.Type(),
		PublicKey:   ssh.MarshalAuthorizedKey(sshPublic),
		PrivateKey:  sealed,
		KeyID:       sealingKeyID,
		Fingerprint: ssh.FingerprintSHA256(sshPublic),
	})
	if err != nil {
		return nil, "", fmt.Errorf("link: storing a key: %w", err)
	}
	key, err := r.open(row)
	return key, sealingKeyID, err
}

// open turns a row into a Key, opening its private half.
func (r *Ring) open(row store.RookeryKeys) (*Key, error) {
	private, err := r.sealed.Open(row.PrivateKey, row.KeyID, []byte(row.ID))
	if err != nil {
		return nil, fmt.Errorf("link: opening the key %s: %w", row.ID, err)
	}
	signer, err := ssh.ParsePrivateKey(private)
	if err != nil {
		return nil, fmt.Errorf("link: parsing the key %s: %w", row.ID, err)
	}
	public := ssh.MarshalAuthorizedKey(signer.PublicKey())
	return &Key{
		ID:          row.ID,
		Name:        row.Name,
		Fingerprint: row.Fingerprint,
		Public:      string(public),
		Record:      row,
		signer:      signer,
	}, nil
}
