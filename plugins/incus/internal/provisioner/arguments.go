package provisioner

import (
	"fmt"
	"strings"

	"github.com/aiviarium/rookery/pkg/sdk"
)

// KindMachine is the core's kind: a machine for a host.
const KindMachine = "machine"

// Instance types, as Incus names them.
const (
	// TypeContainer is a system container.
	TypeContainer = "incus/container"

	// TypeVM is a KVM guest.
	TypeVM = "incus/vm"
)

// DefaultSocket is where Incus listens on the machine that runs it.
const DefaultSocket = "/var/lib/incus/unix.socket"

// DefaultImage is the image a machine is made from when an entry names none.
const DefaultImage = "images:ubuntu/24.04"

// ImageServer is where the images an entry names come from.
const ImageServer = "https://images.linuxcontainers.org"

// arguments are one entry's arguments, CBOR in Core Deterministic mode.
type arguments struct {
	// Image is "remote:alias", such as images:ubuntu/24.04.
	Image string `cbor:"image"`

	// Size is one of the sizes the description offers.
	Size string `cbor:"size"`

	// Profile is the Incus profile the machine takes.
	Profile string `cbor:"profile"`

	// Project is the Incus project the machine is made in.
	Project string `cbor:"project"`

	// Pool is the storage pool the machine's disk comes from.
	Pool string `cbor:"pool"`

	// Socket is the Incus socket on a node.
	Socket string `cbor:"socket"`
}

// parseArguments reads an entry's arguments, with their defaults.
func parseArguments(encoded []byte) (arguments, error) {
	parsed := arguments{}
	if len(encoded) > 0 {
		if err := sdk.Unmarshal(encoded, &parsed); err != nil {
			return arguments{}, fmt.Errorf("the arguments are not an object: %w", err)
		}
	}
	if parsed.Image == "" {
		parsed.Image = DefaultImage
	}
	if parsed.Size == "" {
		parsed.Size = "small"
	}
	if parsed.Profile == "" {
		parsed.Profile = "default"
	}
	if parsed.Pool == "" {
		parsed.Pool = "default"
	}
	return parsed, nil
}

// socket is the Incus socket on a node.
func (a arguments) socket() string {
	if a.Socket == "" {
		return DefaultSocket
	}
	return a.Socket
}

// image splits an image into the remote it comes from and its alias, which is
// how Incus names a remote image: "images:ubuntu/24.04".
func (a arguments) image() (server, alias, protocol string) {
	remote, alias, found := strings.Cut(a.Image, ":")
	if !found {
		return ImageServer, a.Image, "simplestreams"
	}
	switch remote {
	case "images":
		return ImageServer, alias, "simplestreams"
	case "":
		return ImageServer, alias, "simplestreams"
	default:
		// A local image on the server: an alias or a fingerprint, with no
		// remote behind it.
		if strings.Contains(remote, ".") || strings.Contains(remote, "/") {
			return "https://" + remote, alias, "simplestreams"
		}
		return "", a.Image, "local"
	}
}

// size is a machine size the arguments name.
func (a arguments) size() (size, error) {
	sizes := map[string]size{
		"small":  {cpus: 2, memoryMB: 2048, diskGB: 20},
		"medium": {cpus: 4, memoryMB: 8192, diskGB: 50},
		"large":  {cpus: 8, memoryMB: 16384, diskGB: 100},
	}
	chosen, ok := sizes[a.Size]
	if !ok {
		return size{}, fmt.Errorf("%q is not a machine size: small, medium or large", a.Size)
	}
	return chosen, nil
}

// size is a machine's CPU, memory and disk.
type size struct {
	cpus     uint32
	memoryMB uint64
	diskGB   uint64
}

// validName reports whether a name is an Incus name: letters, digits and
// dashes, and no leading digit.
func validName(name string) bool {
	if name == "" || len(name) > 63 {
		return false
	}
	for index, c := range name {
		switch {
		case c >= 'a' && c <= 'z', c >= '0' && c <= '9', c >= 'A' && c <= 'Z', c == '-':
		default:
			return false
		}
		if index == 0 && (c < 'a' || c > 'z') && (c < 'A' || c > 'Z') {
			return false
		}
	}
	return true
}
