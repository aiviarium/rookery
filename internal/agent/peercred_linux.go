//go:build linux

package agent

import (
	"errors"
	"fmt"
	"net"
	"syscall"
)

// peerCredentials reads a Unix connection's peer uid and gid with SO_PEERCRED.
func peerCredentials(conn net.Conn) (uid, gid uint32, err error) {
	unixConn, ok := conn.(*net.UnixConn)
	if !ok {
		return 0, 0, fmt.Errorf("agent: %T is not a Unix connection", conn)
	}
	raw, err := unixConn.SyscallConn()
	if err != nil {
		return 0, 0, err
	}
	var cred *syscall.Ucred
	if err := raw.Control(func(fd uintptr) {
		cred, err = syscall.GetsockoptUcred(int(fd), syscall.SOL_SOCKET, syscall.SO_PEERCRED) //nolint:gosec // the descriptor is the connection's.
	}); err != nil {
		return 0, 0, err
	}
	if cred == nil {
		return 0, 0, errors.New("agent: the connection has no peer credentials")
	}
	return cred.Uid, cred.Gid, nil
}
