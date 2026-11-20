package tailnet

import (
	"context"
	"errors"
	"fmt"
	"net/url"
	"slices"
	"strings"
	"time"

	tsclient "tailscale.com/client/tailscale/v2"
)

// TailscaleOptions configure Tailscale's hosted control server.
type TailscaleOptions struct {
	// APIKey is an API key or an OAuth client secret from the admin console.
	APIKey string
	// Tailnet is the tailnet's name; empty uses the key's own default tailnet.
	Tailnet string
	// BaseURL overrides the API's address, for a test or a proxy.
	BaseURL string
}

// Tailscale is Control for Tailscale's hosted service, through its v2 API client.
type Tailscale struct {
	client *tsclient.Client
}

// NewTailscale makes a Tailscale control server client.
func NewTailscale(options TailscaleOptions) (*Tailscale, error) {
	if options.APIKey == "" {
		return nil, fmt.Errorf("%w: Tailscale needs an API key", ErrRefused)
	}
	client := &tsclient.Client{APIKey: options.APIKey, Tailnet: options.Tailnet}
	if options.BaseURL != "" {
		parsed, err := url.Parse(options.BaseURL)
		if err != nil {
			return nil, fmt.Errorf("tailnet: Tailscale's API address: %w", err)
		}
		client.BaseURL = parsed
	}
	return &Tailscale{client: client}, nil
}

// MintKey makes a one-use, pre-authorized key for a host.
func (t *Tailscale) MintKey(ctx context.Context, request KeyRequest) (Key, error) {
	if request.Hostname == "" {
		return Key{}, fmt.Errorf("%w: a key needs the host's name", ErrRefused)
	}
	expiry := request.Expiry
	if expiry <= 0 {
		expiry = DefaultKeyExpiry
	}
	capabilities := tsclient.KeyCapabilities{}
	capabilities.Devices.Create.Reusable = false
	capabilities.Devices.Create.Ephemeral = request.Ephemeral
	capabilities.Devices.Create.Preauthorized = true
	capabilities.Devices.Create.Tags = request.Tags

	key, err := t.client.Keys().CreateAuthKey(ctx, tsclient.CreateKeyRequest{
		Capabilities:  capabilities,
		ExpirySeconds: int64(expiry / time.Second),
		Description:   "rookery host " + request.Hostname,
	})
	if err != nil {
		return Key{}, wrap("minting a key", err)
	}
	return Key{ID: key.ID, Secret: key.Key, Expires: key.Expires}, nil
}

// Device looks a device up by the hostname it joined with.
func (t *Tailscale) Device(ctx context.Context, hostname string) (Device, error) {
	devices, err := t.client.Devices().List(ctx)
	if err != nil {
		return Device{}, wrap("listing the tailnet's devices", err)
	}
	for _, device := range devices {
		if matchesHostname(device.Hostname, device.Name, hostname) {
			return fromTailscale(device), nil
		}
	}
	return Device{}, fmt.Errorf("%w: no device named %q", ErrNotFound, hostname)
}

// DeleteDevice removes a device from the tailnet.
func (t *Tailscale) DeleteDevice(ctx context.Context, id string) error {
	err := t.client.Devices().Delete(ctx, id)
	if err != nil {
		if apiStatus(err) == 404 {
			return fmt.Errorf("%w: device %q", ErrNotFound, id)
		}
		return wrap("removing a device", err)
	}
	return nil
}

// Devices lists the tailnet's tagged devices.
func (t *Tailscale) Devices(ctx context.Context) ([]Device, error) {
	devices, err := t.client.Devices().List(ctx)
	if err != nil {
		return nil, wrap("listing the tailnet's devices", err)
	}
	tagged := make([]Device, 0, len(devices))
	for _, device := range devices {
		if len(device.Tags) > 0 {
			tagged = append(tagged, fromTailscale(device))
		}
	}
	return tagged, nil
}

// fromTailscale is a device in our own terms.
func fromTailscale(device tsclient.Device) Device {
	converted := Device{
		ID:        device.NodeID,
		Hostname:  device.Hostname,
		Addresses: device.Addresses,
		Tags:      device.Tags,
		Online:    device.ConnectedToControl,
	}
	if converted.ID == "" {
		converted.ID = device.ID
	}
	if device.LastSeen != nil {
		converted.LastSeen = device.LastSeen.Time
	}
	// The v4 address first, which is what a link dials.
	slices.SortStableFunc(converted.Addresses, func(a, b string) int {
		return strings.Count(b, ".") - strings.Count(a, ".")
	})
	return converted
}

// matchesHostname compares a control server's names for a device with the
// hostname Rookery knows it by. Tailscale's Name carries the tailnet's domain,
// which is not part of the host's name.
func matchesHostname(hostname, name, want string) bool {
	if strings.EqualFold(hostname, want) {
		return true
	}
	short, _, found := strings.Cut(name, ".")
	return found && strings.EqualFold(short, want)
}

// wrap says what the control server answered, keeping its error for errors.Is
// and errors.As.
func wrap(doing string, err error) error {
	switch {
	case err == nil:
		return nil
	case apiStatus(err) == 401 || apiStatus(err) == 403:
		return fmt.Errorf("%w: %s: %w", ErrRefused, doing, err)
	case apiStatus(err) == 404:
		return fmt.Errorf("%w: %s: %w", ErrNotFound, doing, err)
	}
	return fmt.Errorf("tailnet: %s: %w", doing, err)
}

// apiStatus is the HTTP status a control server's error carries, or zero.
func apiStatus(err error) int {
	var api *tsclient.APIError
	if errors.As(err, &api) {
		return api.Status
	}
	var response *Error
	if errors.As(err, &response) {
		return response.Status
	}
	return 0
}
