// Package tailnet is the server's place in a tailnet.
//
// It has three parts. Control is a tailnet's control server as the server needs
// it: minting a one-use, pre-authorized, tagged key for each host that joins,
// looking a device up by the hostname it joined with, removing it, and listing
// the devices that carry a tag. Tailscale's hosted service and a self-hosted
// Headscale are both Control. Dialer is the tailnet's network, which Node is
// through tsnet: links reach hosts at their tailnet address and Ansible's SSH
// reaches them through `rookery server dial`. Store keeps the node's state in
// Postgres, so a standby that takes over has the same identity.
//
// Tests use FakeControl and FakeDialer; nothing in the core's tests talks to a
// real control server.
package tailnet

import (
	"context"
	"errors"
	"net"
	"slices"
	"strings"
	"time"
)

// Errors.
var (
	// ErrNotFound is what a control server answers when it has no device or key
	// by that name.
	ErrNotFound = errors.New("tailnet: not found")
	// ErrNoState is what a Store answers when it holds no state for a key.
	ErrNoState = errors.New("tailnet: no state")
	// ErrRefused is what a control server answers when it will not do a thing:
	// a key with tags the API key may not set, a device it has no business
	// removing.
	ErrRefused = errors.New("tailnet: refused")
	// ErrUnreachable is what a control server answers when it does not answer.
	ErrUnreachable = errors.New("tailnet: the control server is unreachable")
)

// DefaultKeyExpiry is how long a host's one-use key may be used when a request
// names no expiry. It is a day, which is longer than the gap between minting a
// key and applying the host.
const DefaultKeyExpiry = 24 * time.Hour

// KeyRequest is a one-use auth key a host joins the tailnet with.
type KeyRequest struct {
	// Hostname is the name the host takes on the tailnet, which is its name in
	// Rookery, so that the device it becomes is looked up by it.
	Hostname string
	// Tags are the tags the key authorizes on the device, such as
	// "tag:rookery".
	Tags []string
	// Ephemeral marks the device for removal when it disconnects. Hosts are not
	// ephemeral: Rookery removes them.
	Ephemeral bool
	// Expiry bounds the key's use. Zero is DefaultKeyExpiry.
	Expiry time.Duration
}

// Key is a minted auth key.
type Key struct {
	// ID is the control server's id for the key.
	ID string
	// Secret is what the host joins with. It is a secret: it goes to the host in
	// vars.json, never to a log or a command line.
	Secret string
	// Expires is when the key stops working.
	Expires time.Time
}

// Device is a machine on the tailnet.
type Device struct {
	// ID is the control server's id for the device.
	ID string
	// Hostname is the name it joined with.
	Hostname string
	// Addresses are its tailnet addresses, IPv4 first.
	Addresses []string
	// Tags are the tags it carries.
	Tags []string
	// Online says whether it is connected now.
	Online bool
	// LastSeen is when it last was.
	LastSeen time.Time
}

// Address is the address to dial the device at: its first IPv4 tailnet address,
// or its first address when it has no IPv4 one, or empty when it has none.
func (d Device) Address() string {
	for _, address := range d.Addresses {
		if strings.Count(address, ".") == 3 {
			return address
		}
	}
	if len(d.Addresses) > 0 {
		return d.Addresses[0]
	}
	return ""
}

// Tagged reports whether the device carries any of the tags.
func (d Device) Tagged(tags []string) bool {
	if len(tags) == 0 {
		return len(d.Tags) > 0
	}
	for _, tag := range tags {
		if slices.Contains(d.Tags, tag) {
			return true
		}
	}
	return false
}

// Control is a tailnet's control server.
type Control interface {
	// MintKey makes a one-use, pre-authorized key for one host. The key carries
	// the tags the request names and no others.
	MintKey(ctx context.Context, request KeyRequest) (Key, error)
	// Device looks a device up by the hostname it joined with. It answers
	// ErrNotFound when the host has not joined yet, which is what an apply that
	// has only just handed the key over sees.
	Device(ctx context.Context, hostname string) (Device, error)
	// DeleteDevice removes a device from the tailnet. It answers ErrNotFound
	// when the device is gone already.
	DeleteDevice(ctx context.Context, id string) error
	// Devices lists the devices that carry a tag: the ones Rookery made, or
	// made and forgot.
	Devices(ctx context.Context) ([]Device, error)
}

// Dialer is a tailnet's network.
type Dialer interface {
	// Dial opens a connection to an address on the tailnet. It takes the
	// networks net.Dialer does: tcp, tcp4, tcp6, udp, udp4 and udp6.
	Dial(ctx context.Context, network, address string) (net.Conn, error)
}
