package link

import (
	"io"

	"golang.org/x/crypto/ssh"
)

// sshPipeWriter ends the dedicated SSH connection when its HTTP/2 transport
// closes. SSH session StdinPipe.Close sends channel EOF and can race with an
// active HTTP/2 write. Closing the underlying client also wakes blocked reads
// and writes without touching the channel's write-side state.
type sshPipeWriter struct {
	io.Writer
	client *ssh.Client
}

func (p sshPipeWriter) Close() error { return p.client.Close() }
