//go:build !linux

package agent

import (
	"errors"
	"net"
)

// peerCredentials is only for a Linux host: a daemon elsewhere would use its
// own way of naming the caller.
func peerCredentials(net.Conn) (uid, gid uint32, err error) {
	return 0, 0, errors.New("agent: peer credentials are only read on Linux")
}
