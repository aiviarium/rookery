package tailnet

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/url"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/jackc/pgx/v5"
	"tailscale.com/ipn"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/seal"
	"github.com/aiviarium/rookery/internal/store"
)

const (
	hostedControl = "https://controlplane.tailscale.com"
)

// ManagerOptions configure the leader's tailnet node.
type ManagerOptions struct {
	Store         *store.Store
	Ring          *seal.Ring
	Dir           string
	Hostname      string
	Tailnet       string
	HeadscaleUser string
	Log           *slog.Logger
}

type deployment struct {
	control    Control
	controlURL string
	tag        string
	node       *Node
}

// Manager owns the configured control client and the leader's node.
type Manager struct {
	options   ManagerOptions
	updates   chan struct{}
	ready     chan struct{}
	readyOnce sync.Once
	mu        sync.RWMutex
	current   deployment
}

// NewManager constructs a tailnet that is idle until Load or Configure.
func NewManager(options ManagerOptions) *Manager {
	if options.Hostname == "" {
		options.Hostname = "rookery-server"
	}
	return &Manager{options: options, updates: make(chan struct{}, 1), ready: make(chan struct{})}
}

func (m *Manager) snapshot() deployment {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.current
}

func (m *Manager) set(value deployment) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.current = value
	if value.node != nil {
		m.readyOnce.Do(func() { close(m.ready) })
	}
}

// Ready closes when the leader's node is up.
func (m *Manager) Ready() <-chan struct{} { return m.ready }

func (m *Manager) lock(ctx context.Context) (func(), error) {
	select {
	case m.updates <- struct{}{}:
		return func() { <-m.updates }, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

// Load starts the persisted node identity. An unconfigured deployment has no
// node and makes no network requests.
func (m *Manager) Load(ctx context.Context) error {
	row, err := m.options.Store.Queries().GetTailnetConfig(ctx)
	if errors.Is(err, pgx.ErrNoRows) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("tailnet: loading configuration: %w", err)
	}
	key, err := m.options.Ring.Open(row.ApiKey, row.KeyID, []byte(seal.TailnetControlAAD))
	if err != nil {
		return fmt.Errorf("tailnet: opening API credentials: %w", err)
	}
	value, err := m.prepare(ctx, row.ControlUrl, row.Tag, string(key))
	if err != nil {
		return err
	}
	m.set(value)
	return nil
}

// Configure joins the selected tailnet or replaces the control credentials.
// Changing control servers requires a new deployment identity.
func (m *Manager) Configure(ctx context.Context, controlURL, tag, apiKey string) error {
	controlURL = strings.TrimRight(strings.TrimSpace(controlURL), "/")
	if controlURL == "" {
		controlURL = hostedControl
	}
	parsed, err := url.Parse(controlURL)
	if err != nil || parsed.Scheme != "https" || parsed.Hostname() == "" ||
		parsed.User != nil || parsed.RawQuery != "" || parsed.Fragment != "" || parsed.Path != "" {
		return fmt.Errorf("%w: tailnet control URL must be an HTTPS origin", access.ErrInvalid)
	}
	if ok, _ := regexp.MatchString(`^tag:[a-zA-Z][a-zA-Z0-9-]*$`, tag); !ok || strings.TrimSpace(apiKey) == "" {
		return fmt.Errorf("%w: tailnet configuration needs a tag and API key", access.ErrInvalid)
	}
	unlock, err := m.lock(ctx)
	if err != nil {
		return err
	}
	defer unlock()

	value, err := m.prepare(ctx, controlURL, tag, apiKey)
	if err != nil {
		return err
	}
	current := m.snapshot()
	if current.node == nil {
		defer func() {
			if m.snapshot().node != value.node {
				_ = value.node.Close()
			}
		}()
	}
	encrypted, keyID, err := m.options.Ring.Seal([]byte(apiKey), []byte(seal.TailnetControlAAD))
	if err != nil {
		return fmt.Errorf("tailnet: sealing API credentials: %w", err)
	}
	if err := m.options.Store.Queries().SetTailnetConfig(ctx, store.SetTailnetConfigParams{
		ControlUrl: controlURL, Tag: tag, ApiKey: encrypted, KeyID: keyID,
	}); err != nil {
		return fmt.Errorf("tailnet: saving configuration: %w", err)
	}
	m.set(value)
	return nil
}

func (m *Manager) prepare(ctx context.Context, controlURL, tag, apiKey string) (deployment, error) {
	current := m.snapshot()
	if current.controlURL != "" && current.controlURL != controlURL {
		return deployment{}, fmt.Errorf("%w: the node already belongs to a control server", access.ErrConflict)
	}
	var control Control
	var err error
	if controlURL == hostedControl {
		control, err = NewTailscale(TailscaleOptions{APIKey: apiKey, Tailnet: m.options.Tailnet})
	} else {
		control, err = NewHeadscale(HeadscaleOptions{URL: controlURL, APIKey: apiKey, User: m.options.HeadscaleUser})
	}
	if err != nil {
		return deployment{}, fmt.Errorf("%w: configuring tailnet control: %w", access.ErrInvalid, err)
	}
	node := current.node
	if node == nil {
		node, err = m.startNode(ctx, controlURL, tag, control)
		if err != nil {
			return deployment{}, err
		}
	}
	return deployment{control: control, controlURL: controlURL, tag: tag, node: node}, nil
}

func (m *Manager) startNode(ctx context.Context, controlURL, tag string, control Control) (*Node, error) {
	ctx, cancel := context.WithTimeout(ctx, 45*time.Second)
	defer cancel()

	state := NewPostgresStore(m.options.Store.Pool())
	var auth string
	if _, err := state.Get(ctx, string(ipn.CurrentProfileStateKey)); errors.Is(err, ErrNoState) {
		key, err := control.MintKey(ctx, KeyRequest{Hostname: m.options.Hostname, Tags: []string{tag}})
		if err != nil {
			return nil, fmt.Errorf("%w: could not authorize the server on the tailnet", access.ErrUnavailable)
		}
		auth = key.Secret
	} else if err != nil {
		return nil, err
	}
	node, err := NewNode(NodeOptions{
		Store: state, Dir: m.options.Dir, Hostname: m.options.Hostname,
		ControlURL: controlURL, AuthKey: auth, Log: m.options.Log,
	})
	if err != nil {
		return nil, err
	}
	if err := node.Start(ctx); err != nil {
		_ = node.Close()
		return nil, fmt.Errorf("%w: could not start the tailnet node", access.ErrUnavailable)
	}
	return node, nil
}

// Status reads the leader's configured tailnet and node addresses.
func (m *Manager) Status() *rookeryv1.TailnetServiceStatusResponse {
	state := m.snapshot()
	out := &rookeryv1.TailnetServiceStatusResponse{
		Configured: state.control != nil, ControlUrl: state.controlURL, Tag: state.tag,
	}
	if state.node != nil {
		out.Addresses, _ = state.node.Addresses()
		out.Online = len(out.Addresses) > 0
	}
	return out
}

// MintHostKey gives a joining host a single-use key with the deployment's tag.
func (m *Manager) MintHostKey(ctx context.Context, host string) (Key, error) {
	state := m.snapshot()
	if state.control == nil {
		return Key{}, fmt.Errorf("%w: the tailnet is not configured", access.ErrUnavailable)
	}
	return state.control.MintKey(ctx, KeyRequest{Hostname: host, Tags: []string{state.tag}})
}

// Device resolves a host's enrolled device with the deployment's tag.
func (m *Manager) Device(ctx context.Context, hostname string) (Device, error) {
	state := m.snapshot()
	if state.control == nil {
		return Device{}, ErrNotFound
	}
	device, err := state.control.Device(ctx, hostname)
	if err != nil {
		return Device{}, err
	}
	if !device.Tagged([]string{state.tag}) {
		return Device{}, ErrNotFound
	}
	return device, nil
}

// DeleteDevice forgets a managed device. An already removed device is harmless.
func (m *Manager) DeleteDevice(ctx context.Context, deviceID string) error {
	state := m.snapshot()
	if state.control == nil {
		return nil
	}
	devices, err := m.Devices(ctx)
	if err != nil {
		return err
	}
	for _, device := range devices {
		if device.ID == deviceID {
			err := state.control.DeleteDevice(ctx, deviceID)
			if !errors.Is(err, ErrNotFound) {
				return err
			}
			return nil
		}
	}
	return nil
}

// Devices returns the tagged host devices and excludes the leader's own node.
func (m *Manager) Devices(ctx context.Context) ([]Device, error) {
	state := m.snapshot()
	if state.control == nil {
		return nil, nil
	}
	values, err := state.control.Devices(ctx)
	if err != nil {
		return nil, fmt.Errorf("%w: could not list tailnet devices", access.ErrUnavailable)
	}
	result := make([]Device, 0, len(values))
	for _, device := range values {
		if !device.Tagged([]string{state.tag}) || device.Hostname == m.options.Hostname {
			continue
		}
		result = append(result, device)
	}
	return result, nil
}

// Dial opens a connection through the leader's node.
func (m *Manager) Dial(ctx context.Context, network, address string) (net.Conn, error) {
	node := m.snapshot().node
	if node == nil {
		return nil, fmt.Errorf("%w: the tailnet is not configured", access.ErrUnavailable)
	}
	return node.Dial(ctx, network, address)
}

// Listen serves on the leader's node. An unconfigured deployment has no listener.
func (m *Manager) Listen(network, address string) (net.Listener, error) {
	node := m.snapshot().node
	if node == nil {
		return nil, nil
	}
	return node.Listen(network, address)
}

// Close stops the leader's node.
func (m *Manager) Close() error {
	node := m.snapshot().node
	if node == nil {
		return nil
	}
	return node.Close()
}
