package provisioner

import (
	"fmt"
	"strings"

	"github.com/aiviarium/rookery/pkg/sdk"
)

// Kinds of resource the Docker provisioner makes.
const (
	// KindMachine is the core's kind: a machine for a host.
	KindMachine = "machine"

	// KindDeployment is a compose project.
	KindDeployment = "deployment"
)

// arguments are one entry's arguments, CBOR in Core Deterministic mode.
type arguments struct {
	// Image is the image a machine runs.
	Image string `cbor:"image"`

	// Size is one of the sizes the description offers.
	Size string `cbor:"size"`

	// Runtime is the container runtime a machine uses. Empty picks sysbox-runc
	// when the engine has it, and the engine's own runtime otherwise.
	Runtime string `cbor:"runtime"`

	// Network is the Docker network machines and deployments join.
	Network string `cbor:"network"`

	// Socket is the engine's socket on a node.
	Socket string `cbor:"socket"`

	// File and Content are a deployment's compose file: a path on the server's
	// machine, or the YAML itself.
	File    string `cbor:"file"`
	Content string `cbor:"content"`

	// Project is the compose project's name. Empty is the resource's name.
	Project string `cbor:"project"`

	// Environment are the values a compose file's variables take.
	Environment map[string]string `cbor:"environment"`
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
		parsed.Image = DefaultMachineImage
	}
	if parsed.Size == "" {
		parsed.Size = "small"
	}
	if parsed.Network == "" {
		parsed.Network = DefaultNetwork
	}
	return parsed, nil
}

// socket is the engine's socket on a node.
func (a arguments) socket() string {
	if a.Socket == "" {
		return DefaultSocket
	}
	return a.Socket
}

// size is the machine size the arguments name.
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
	cpus     float32
	memoryMB int64
	diskGB   int64
}

// memory is the memory limit in bytes.
func (s size) memory() int64 { return s.memoryMB << 20 }

// nanoCPUs is the CPU limit as Docker counts it.
func (s size) nanoCPUs() int64 {
	return int64(s.cpus * 1e9)
}

// validName reports whether a name is a Docker name: a DNS label to Docker's
// rules, which container names and volume names must be.
func validName(name string) bool {
	if name == "" || len(name) > 63 {
		return false
	}
	for _, c := range name {
		switch {
		case c >= 'a' && c <= 'z', c >= '0' && c <= '9', c == '-', c == '_', c == '.':
		default:
			return false
		}
	}
	return true
}

// dockerName is a name made of parts, with everything Docker does not accept
// replaced: an entry's name holds its type, and a type holds a slash.
func dockerName(parts ...string) string {
	joined := strings.Join(parts, "-")
	var out strings.Builder
	for _, c := range joined {
		switch {
		case c >= 'a' && c <= 'z', c >= '0' && c <= '9', c == '-', c == '_', c == '.':
			out.WriteRune(c)
		case c >= 'A' && c <= 'Z':
			out.WriteRune(c + ('a' - 'A'))
		default:
			out.WriteByte('-')
		}
	}
	name := strings.Trim(out.String(), "-.")
	if len(name) > 63 {
		name = name[:63]
	}
	return name
}
