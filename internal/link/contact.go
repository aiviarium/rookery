package link

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"strings"

	"golang.org/x/crypto/ssh"

	"github.com/aiviarium/rookery/internal/access"
)

// Credential is how first contact logs in when the ring's key is refused: a
// password or a private key, given once, used once, and never stored.
type Credential struct {
	// Password is the account's password.
	Password string
	// PrivateKey is a PEM private key, unencrypted.
	PrivateKey []byte
}

// Empty reports whether there is no credential.
func (c Credential) Empty() bool { return c.Password == "" && len(c.PrivateKey) == 0 }

// auth is the credential as x/crypto/ssh wants it.
func (c Credential) auth() (ssh.AuthMethod, error) {
	if c.Password != "" {
		return ssh.Password(c.Password), nil
	}
	if len(c.PrivateKey) > 0 {
		signer, err := ssh.ParsePrivateKey(c.PrivateKey)
		if err != nil {
			return nil, fmt.Errorf("%w: reading the private key: %w", access.ErrInvalid, err)
		}
		return ssh.PublicKeys(signer), nil
	}
	return nil, fmt.Errorf("%w: a credential is a password or a private key", access.ErrInvalid)
}

// InstallScript adds a line from stdin to the account's authorized_keys,
// without interpolating it into a shell string. It creates the file and its
// directory with the modes sshd wants, and is safe to run twice.
const InstallScript = `umask 077
mkdir -p "$HOME/.ssh"
chmod 700 "$HOME/.ssh"
line=$(cat)
touch "$HOME/.ssh/authorized_keys"
grep -qxF -- "$line" "$HOME/.ssh/authorized_keys" || printf '%s\n' "$line" >> "$HOME/.ssh/authorized_keys"
chmod 600 "$HOME/.ssh/authorized_keys"
`

// Contact is what first contact found and did.
type Contact struct {
	// Fingerprint is the host key's, as the server recorded it.
	Fingerprint string
	// KeyWorked says the ring's key logged in: nothing was installed.
	KeyWorked bool
	// Installed says the ring's public key was added to the account's
	// authorized_keys.
	Installed bool
	// Message says in words what happened, for the CLI.
	Message string
}

// Contact logs in to a host: with the ring's key first, and when that is
// refused, with a credential, installing the ring's public key so that every
// later link uses the ring. The host key is trusted on first use, and a key
// other than the trusted one fails the contact.
func (m *Manager) Contact(ctx context.Context, target Target, credential Credential, log *slog.Logger) (Contact, error) {
	if log == nil {
		log = m.options.Log
	}
	key, err := m.ring.Active(ctx)
	if err != nil {
		return Contact{}, err
	}
	hostKey := m.known.Callback(ctx, target.knownAddress(), target.Name)

	client, err := m.dialSSH(ctx, target, ssh.PublicKeys(key.signer), hostKey)
	switch {
	case err == nil:
		defer func() { _ = client.Close() }()
		if err := runCheck(ctx, client); err != nil {
			return Contact{}, err
		}
		return m.finish(ctx, target, Contact{KeyWorked: true, Message: "the key ring's key logs in"}, log)
	case errors.Is(err, ErrMismatch):
		return Contact{}, err
	case credential.Empty():
		return Contact{}, fmt.Errorf("%w: the key ring's key was refused: give a password or a private key once (--password-stdin or --key FILE) and it is installed",
			access.ErrUnavailable)
	}

	auth, err := credential.auth()
	if err != nil {
		return Contact{}, err
	}
	client, err = m.dialSSH(ctx, target, auth, hostKey)
	if err != nil {
		return Contact{}, err
	}
	defer func() { _ = client.Close() }()

	session, err := client.NewSession()
	if err != nil {
		return Contact{}, fmt.Errorf("link: opening a session on %s: %w", target.Name, err)
	}
	defer func() { _ = session.Close() }()
	session.Stdin = strings.NewReader(key.Public)
	session.Stdout = io.Discard
	session.Stderr = io.Discard
	if err := session.Run(InstallScript); err != nil {
		return Contact{}, fmt.Errorf("%w: installing the key ring's key on %s: %w", access.ErrUnavailable, target.Name, err)
	}

	// The credential worked and the key is installed. Log in with the ring to
	// prove it, which is also the first call that will later open a link.
	verify, err := m.dialSSH(ctx, target, ssh.PublicKeys(key.signer), hostKey)
	if err != nil {
		return Contact{}, fmt.Errorf("%w: the key ring's key still does not log in to %s: %w", access.ErrUnavailable, target.Name, err)
	}
	defer func() { _ = verify.Close() }()
	if err := runCheck(ctx, verify); err != nil {
		return Contact{}, err
	}
	return m.finish(ctx, target, Contact{Installed: true, Message: "installed the key ring's key"}, log)
}

// runCheck runs one trivial command, so that a login which works is known to
// give a shell and not only a handshake.
func runCheck(_ context.Context, client *ssh.Client) error {
	session, err := client.NewSession()
	if err != nil {
		return fmt.Errorf("link: opening a session: %w", err)
	}
	defer func() { _ = session.Close() }()
	if err := session.Run("true"); err != nil {
		return fmt.Errorf("link: running a command: %w", err)
	}
	return nil
}

// finish reads the host key it recorded and mentions it.
func (m *Manager) finish(ctx context.Context, target Target, contact Contact, log *slog.Logger) (Contact, error) {
	row, err := m.opened.Queries().GetKnownHost(ctx, target.knownAddress())
	if err == nil {
		contact.Fingerprint = row.Fingerprint
	}
	log.Info("contacted a host",
		slog.String("host", target.Name),
		slog.String("address", target.knownAddress()),
		slog.String("fingerprint", contact.Fingerprint),
		slog.Bool("installed", contact.Installed))
	return contact, nil
}

// ErrNotManaged reports a link asked for on a host that has no rookeryd.
var ErrNotManaged = errors.New("link: the host has no rookeryd: apply the base role first")
