package agent

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"
)

// socketRole is which of the two sockets a call came in on.
type socketRole int

const (
	// roleRoot is the socket the server uses, mode 0600, for every account.
	roleRoot socketRole = iota + 1
	// roleUser is the socket local programs use, mode 0666, for what the
	// caller's own account may do.
	roleUser
)

// callerKey is the context key the connection's caller is stored under.
type callerKey struct{}

// caller is who a call is from: the peer's uid and account, and which socket it
// came in on.
type caller struct {
	role    socketRole
	uid     uint32
	gid     uint32
	account *account
	err     error
}

// callerOf returns the caller of the call being handled.
func callerOf(ctx context.Context) (*caller, error) {
	caller, ok := ctx.Value(callerKey{}).(*caller)
	if !ok || caller == nil {
		return nil, unauthenticated(errors.New("agent: the call names no connection"))
	}
	if caller.err != nil {
		return nil, unauthenticated(caller.err)
	}
	return caller, nil
}

// accountFor resolves the account a call acts as.
//
// The root socket may name any account; an empty name there is root. The user
// socket may only be its own account, whatever the call asks for.
func accountFor(ctx context.Context, want string) (*account, *caller, error) {
	caller, err := callerOf(ctx)
	if err != nil {
		return nil, nil, err
	}

	if want == "" || want == caller.account.Name {
		return caller.account, caller, nil
	}
	if caller.role != roleRoot {
		return nil, nil, denied("agent: this socket acts only as %s, not %s", caller.account.Name, want)
	}
	acct, err := lookupAccount(want)
	if err != nil {
		return nil, nil, err
	}
	return acct, caller, nil
}

// listen opens a Unix socket at path with mode, removing a stale one first. The
// parent directory is created when it is missing, which only a test's does.
func listen(ctx context.Context, path string, mode os.FileMode) (net.Listener, error) {
	if err := os.MkdirAll(filepath.Dir(path), 0o750); err != nil {
		return nil, fmt.Errorf("agent: creating %s: %w", filepath.Dir(path), err)
	}
	if err := os.Remove(path); err != nil && !errors.Is(err, os.ErrNotExist) {
		return nil, fmt.Errorf("agent: removing the stale socket %s: %w", path, err)
	}
	listener, err := (&net.ListenConfig{}).Listen(ctx, "unix", path)
	if err != nil {
		return nil, fmt.Errorf("agent: listening on %s: %w", path, err)
	}
	if err := os.Chmod(path, mode); err != nil {
		_ = listener.Close()
		return nil, fmt.Errorf("agent: setting the mode of %s: %w", path, err)
	}
	return listener, nil
}

// cleartextHTTP2 is HTTP/2 with prior knowledge and no TLS, which is what gRPC
// on a Unix socket is.
func cleartextHTTP2() *http.Protocols {
	protocols := new(http.Protocols)
	protocols.SetUnencryptedHTTP2(true)
	return protocols
}
