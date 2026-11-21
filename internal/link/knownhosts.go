package link

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"strconv"

	"github.com/jackc/pgx/v5"
	"golang.org/x/crypto/ssh"
	"golang.org/x/crypto/ssh/knownhosts"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/store"
)

// ErrMismatch reports a host that offered a key other than the one the server
// trusts. Every link to it fails until an admin trusts the new key.
var ErrMismatch = errors.New("link: the host's key changed")

// Mismatch is the error of a host key that is not the trusted one. It names the
// key that was offered, so an admin can trust it by its fingerprint.
type Mismatch struct {
	// Address is the host's address as known_hosts writes it.
	Address string
	// Trusted is the fingerprint the server has.
	Trusted string
	// Offered is the fingerprint the host presented.
	Offered string
}

// Error says what happened and what would change it.
func (m *Mismatch) Error() string {
	return fmt.Sprintf("link: host %s offered %s, not the trusted %s: an admin trusts the new key with `rookery key trust %s`",
		m.Address, m.Offered, m.Trusted, m.Address)
}

// Unwrap makes a Mismatch an ErrMismatch.
func (m *Mismatch) Unwrap() error { return ErrMismatch }

// KnownHost is one host key the server trusts.
type KnownHost struct {
	Address            string
	Host               string
	KeyType            string
	PublicKey          string
	Fingerprint        string
	PendingFingerprint string
	PendingAt          string
	TrustedAt          string
}

// KnownHosts is the host keys the server trusts, by address, kept in the
// database rather than a file: the server's image has no writable $HOME, and a
// standby's server takes over with the same trusts.
//
// A key is trusted on first use, and logged with its fingerprint. A host that
// later offers another key is refused, and the offered key is kept as pending
// until an admin trusts it.
type KnownHosts struct {
	store *store.Store
	log   *slog.Logger
}

// NewKnownHosts returns the store of trusted host keys.
func NewKnownHosts(opened *store.Store, logger *slog.Logger) *KnownHosts {
	if logger == nil {
		logger = slog.Default()
	}
	return &KnownHosts{store: opened, log: logger}
}

// Address is the key an address is trusted under: "host:port", always with the
// port, so two ports on one machine are two entries.
func Address(host string, port int) string {
	if port == 0 {
		port = 22
	}
	return net.JoinHostPort(host, strconv.Itoa(port))
}

// Check trusts the key a host presented, records it on first use, and refuses a
// key other than the trusted one. host is the host's name, recorded with the
// address for a reader.
func (k *KnownHosts) Check(ctx context.Context, address, host string, key ssh.PublicKey) error {
	fingerprint := ssh.FingerprintSHA256(key)
	row, err := k.store.Queries().GetKnownHost(ctx, address)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		created, createErr := k.store.Queries().CreateKnownHost(ctx, store.CreateKnownHostParams{
			Address:     address,
			Host:        host,
			KeyType:     key.Type(),
			PublicKey:   ssh.MarshalAuthorizedKey(key),
			Fingerprint: fingerprint,
		})
		if createErr != nil {
			return fmt.Errorf("link: recording the host key of %s: %w", address, createErr)
		}
		if host != "" && created.Host != host {
			if err := k.store.Queries().SetKnownHostHost(ctx, store.SetKnownHostHostParams{Address: created.Address, Host: host}); err != nil {
				return fmt.Errorf("link: recording the host of %s: %w", address, err)
			}
		}
		k.log.Info("trusted a host key on first use",
			slog.String("host", host),
			slog.String("address", address),
			slog.String("fingerprint", fingerprint))
		return nil
	case err != nil:
		return fmt.Errorf("link: reading the host key of %s: %w", address, err)
	}

	if bytes.Equal(row.PublicKey, ssh.MarshalAuthorizedKey(key)) {
		return nil
	}
	pending := fingerprint
	if err := k.store.Queries().OfferKnownHostKey(ctx, store.OfferKnownHostKeyParams{
		Address:     address,
		KeyType:     pointer(key.Type()),
		PublicKey:   ssh.MarshalAuthorizedKey(key),
		Fingerprint: &pending,
	}); err != nil {
		return fmt.Errorf("link: recording the key %s offered: %w", address, err)
	}
	k.log.Warn("a host offered a key other than the trusted one",
		slog.String("host", host),
		slog.String("address", address),
		slog.String("trusted", row.Fingerprint),
		slog.String("offered", fingerprint))

	// The host's record may have been renamed: keep the address's host current.
	if host != "" && row.Host != host {
		if err := k.store.Queries().SetKnownHostHost(ctx, store.SetKnownHostHostParams{Address: address, Host: host}); err != nil {
			return fmt.Errorf("link: recording the host of %s: %w", address, err)
		}
	}
	return &Mismatch{Address: address, Trusted: row.Fingerprint, Offered: fingerprint}
}

// Callback is the check as x/crypto/ssh wants it.
func (k *KnownHosts) Callback(ctx context.Context, address, host string) ssh.HostKeyCallback {
	return func(_ string, _ net.Addr, key ssh.PublicKey) error {
		return k.Check(ctx, address, host, key)
	}
}

// List returns the trusted host keys.
func (k *KnownHosts) List(ctx context.Context) ([]KnownHost, error) {
	rows, err := k.store.Queries().ListKnownHosts(ctx)
	if err != nil {
		return nil, fmt.Errorf("link: listing the trusted host keys: %w", err)
	}
	hosts := make([]KnownHost, 0, len(rows))
	for _, row := range rows {
		known := KnownHost{
			Address:     row.Address,
			Host:        row.Host,
			KeyType:     row.KeyType,
			PublicKey:   string(row.PublicKey),
			Fingerprint: row.Fingerprint,
			TrustedAt:   row.TrustedAt.UTC().Format("2006-01-02T15:04:05Z07:00"),
		}
		if row.PendingFingerprint != nil {
			known.PendingFingerprint = *row.PendingFingerprint
		}
		if row.PendingAt != nil {
			known.PendingAt = row.PendingAt.UTC().Format("2006-01-02T15:04:05Z07:00")
		}
		hosts = append(hosts, known)
	}
	return hosts, nil
}

// Trust makes the pending key of an address the trusted one. The fingerprint
// must be the one the host offered, so a mismatch cannot be trusted blind.
func (k *KnownHosts) Trust(ctx context.Context, address, fingerprint string) (KnownHost, error) {
	row, err := k.store.Queries().TrustKnownHostPending(ctx, store.TrustKnownHostPendingParams{
		Address:     address,
		Fingerprint: &fingerprint,
	})
	if err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return KnownHost{}, fmt.Errorf("%w: %s did not offer the key %s", access.ErrInvalid, address, fingerprint)
		}
		return KnownHost{}, fmt.Errorf("link: trusting the key of %s: %w", address, err)
	}
	return KnownHost{
		Address:     row.Address,
		Host:        row.Host,
		KeyType:     row.KeyType,
		PublicKey:   string(row.PublicKey),
		Fingerprint: row.Fingerprint,
		TrustedAt:   row.TrustedAt.UTC().Format("2006-01-02T15:04:05Z07:00"),
	}, nil
}

// Forget deletes an address's trusted key, so the next link trusts what it is
// offered.
func (k *KnownHosts) Forget(ctx context.Context, address string) error {
	deleted, err := k.store.Queries().DeleteKnownHost(ctx, address)
	if err != nil {
		return fmt.Errorf("link: forgetting the key of %s: %w", address, err)
	}
	if deleted == 0 {
		return fmt.Errorf("%w: no trusted key for %s", access.ErrNotFound, address)
	}
	return nil
}

// ForgetHost deletes every key trusted for a host's addresses, for a host whose
// machine is gone: a machine made again, perhaps at the same address, has keys
// of its own. A host with no trusted key is not an error.
func (k *KnownHosts) ForgetHost(ctx context.Context, host string) error {
	if host == "" {
		return nil
	}
	if _, err := k.store.Queries().DeleteKnownHostsOfHost(ctx, host); err != nil {
		return fmt.Errorf("link: forgetting the keys of %s: %w", host, err)
	}
	return nil
}

// Line is a host's known_hosts line, as an apply writes it for Ansible:
// "[address]:port key-type key". A host with no trusted key is an error: an
// apply never trusts a key it has not seen.
func (k *KnownHosts) Line(ctx context.Context, address string) ([]byte, error) {
	row, err := k.store.Queries().GetKnownHost(ctx, address)
	if err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return nil, fmt.Errorf("%w: no trusted key for %s: contact the host first", access.ErrNotFound, address)
		}
		return nil, fmt.Errorf("link: reading the key of %s: %w", address, err)
	}
	key, _, _, _, err := ssh.ParseAuthorizedKey(row.PublicKey)
	if err != nil {
		return nil, fmt.Errorf("link: the stored key of %s does not parse: %w", address, err)
	}
	return []byte(knownhosts.Line([]string{address}, key) + "\n"), nil
}

// pointer is the address of a value, for an optional column.
func pointer[T any](value T) *T { return &value }
