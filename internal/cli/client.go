package cli

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"

	connectrpc "connectrpc.com/connect"

	rookeryv1connect "github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
)

// defaultSocket is the server's Unix socket where it is not set otherwise.
const defaultSocket = "/run/rookery/rookery.sock"

// clientConfig is what `rookery login` saves.
type clientConfig struct {
	Server string `json:"server"`
	Token  string `json:"token"`
}

// clientConfigPath is where a login is saved: the user's configuration
// directory, so that a token stays out of a repository.
func clientConfigPath() (string, error) {
	dir, err := os.UserConfigDir()
	if err != nil {
		return "", fmt.Errorf("cli: finding the configuration directory: %w", err)
	}
	return filepath.Join(dir, "rookery", "config.json"), nil
}

// loadClientConfig reads the saved session, and answers nothing when there is
// none.
func loadClientConfig() (clientConfig, error) {
	path, err := clientConfigPath()
	if err != nil {
		return clientConfig{}, err
	}
	raw, err := os.ReadFile(path) //nolint:gosec // the user's own configuration
	switch {
	case errors.Is(err, os.ErrNotExist):
		return clientConfig{}, nil
	case err != nil:
		return clientConfig{}, fmt.Errorf("cli: reading %s: %w", path, err)
	}

	var saved clientConfig
	if err := json.Unmarshal(raw, &saved); err != nil {
		return clientConfig{}, fmt.Errorf("cli: reading %s: %w", path, err)
	}
	return saved, nil
}

// saveClientConfig writes the session, mode 0600: it holds a token.
func saveClientConfig(saved clientConfig) error {
	path, err := clientConfigPath()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return fmt.Errorf("cli: making the configuration directory: %w", err)
	}
	raw, err := json.MarshalIndent(saved, "", "  ")
	if err != nil {
		return fmt.Errorf("cli: writing the configuration: %w", err)
	}
	raw = append(raw, '\n')
	if err := os.WriteFile(path, raw, 0o600); err != nil {
		return fmt.Errorf("cli: writing %s: %w", path, err)
	}
	return nil
}

// clearClientConfig forgets the saved session.
func clearClientConfig() error {
	path, err := clientConfigPath()
	if err != nil {
		return err
	}
	if err := os.Remove(path); err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("cli: removing %s: %w", path, err)
	}
	return nil
}

// api is the CLI's clients, one per service.
type api struct {
	Server string

	Auth          rookeryv1connect.AuthServiceClient
	Users         rookeryv1connect.UserServiceClient
	Groups        rookeryv1connect.GroupServiceClient
	Access        rookeryv1connect.AccessServiceClient
	Spaces        rookeryv1connect.SpaceServiceClient
	Audit         rookeryv1connect.AuditServiceClient
	Local         rookeryv1connect.LocalServiceClient
	Jobs          rookeryv1connect.JobServiceClient
	Schedules     rookeryv1connect.ScheduleServiceClient
	Events        rookeryv1connect.EventServiceClient
	Topics        rookeryv1connect.TopicServiceClient
	Notifications rookeryv1connect.NotificationServiceClient
	Settings      rookeryv1connect.SettingServiceClient
	Secrets       rookeryv1connect.SecretServiceClient
	Connections   rookeryv1connect.ConnectionServiceClient
	Blobs         rookeryv1connect.BlobServiceClient
	Plugins       rookeryv1connect.PluginServiceClient
	Registries    rookeryv1connect.RegistryServiceClient
	Hosts         rookeryv1connect.HostServiceClient
	Keys          rookeryv1connect.KeyServiceClient
}

// dial connects to a server at an address: a Unix socket, or an http or https
// URL.
func dial(ctx context.Context, address, token string) (*api, error) {
	httpClient, base, err := transport(address, token)
	if err != nil {
		return nil, err
	}
	_ = ctx

	options := []connectrpc.ClientOption{connectrpc.WithGRPC()}
	return &api{
		Server:        base,
		Auth:          rookeryv1connect.NewAuthServiceClient(httpClient, base, options...),
		Users:         rookeryv1connect.NewUserServiceClient(httpClient, base, options...),
		Groups:        rookeryv1connect.NewGroupServiceClient(httpClient, base, options...),
		Access:        rookeryv1connect.NewAccessServiceClient(httpClient, base, options...),
		Spaces:        rookeryv1connect.NewSpaceServiceClient(httpClient, base, options...),
		Audit:         rookeryv1connect.NewAuditServiceClient(httpClient, base, options...),
		Local:         rookeryv1connect.NewLocalServiceClient(httpClient, base, options...),
		Jobs:          rookeryv1connect.NewJobServiceClient(httpClient, base, options...),
		Schedules:     rookeryv1connect.NewScheduleServiceClient(httpClient, base, options...),
		Events:        rookeryv1connect.NewEventServiceClient(httpClient, base, options...),
		Topics:        rookeryv1connect.NewTopicServiceClient(httpClient, base, options...),
		Notifications: rookeryv1connect.NewNotificationServiceClient(httpClient, base, options...),
		Settings:      rookeryv1connect.NewSettingServiceClient(httpClient, base, options...),
		Secrets:       rookeryv1connect.NewSecretServiceClient(httpClient, base, options...),
		Connections:   rookeryv1connect.NewConnectionServiceClient(httpClient, base, options...),
		Blobs:         rookeryv1connect.NewBlobServiceClient(httpClient, base, options...),
		Plugins:       rookeryv1connect.NewPluginServiceClient(httpClient, base, options...),
		Registries:    rookeryv1connect.NewRegistryServiceClient(httpClient, base, options...),
		Hosts:         rookeryv1connect.NewHostServiceClient(httpClient, base, options...),
		Keys:          rookeryv1connect.NewKeyServiceClient(httpClient, base, options...),
	}, nil
}

// transport is an HTTP client and the base address its calls go to.
func transport(address, token string) (*http.Client, string, error) {
	if path, ok := socketPath(address); ok {
		transport := &http.Transport{
			Protocols: cleartextHTTP2(),
			DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
				var dialer net.Dialer
				return dialer.DialContext(ctx, "unix", path)
			},
			DisableCompression: true,
		}
		return &http.Client{Transport: transport}, "http://rookery", nil
	}

	parsed, err := url.Parse(address)
	if err != nil {
		return nil, "", fmt.Errorf("cli: %q is not a server address: %w", address, err)
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return nil, "", fmt.Errorf("cli: %q is not a server address: use http, https or unix", address)
	}
	parsed.Path = strings.TrimRight(parsed.Path, "/")
	base := parsed.Scheme + "://" + parsed.Host

	transport := http.DefaultTransport.(*http.Transport).Clone()
	transport.DisableCompression = true
	if parsed.Scheme == "http" {
		// A deployment that is not behind TLS speaks h2c.
		transport.Protocols = cleartextHTTP2()
	}
	if token != "" {
		transport = withBearer(transport, token).(*http.Transport)
	}
	return &http.Client{Transport: transport, Timeout: 5 * time.Minute}, base, nil
}

// socketPath is the Unix socket an address names, if it names one.
func socketPath(address string) (string, bool) {
	switch {
	case strings.HasPrefix(address, "unix://"):
		return strings.TrimPrefix(address, "unix://"), true
	case strings.HasPrefix(address, "unix:"):
		return strings.TrimPrefix(address, "unix:"), true
	case strings.HasPrefix(address, "/"):
		return address, true
	default:
		return "", false
	}
}

// bearer is a transport that adds an access token to every call.
type bearer struct {
	next  http.RoundTripper
	token string
}

// RoundTrip adds the Authorization header.
func (b bearer) RoundTrip(request *http.Request) (*http.Response, error) {
	clone := request.Clone(request.Context())
	clone.Header.Set("Authorization", "Bearer "+b.token)
	return b.next.RoundTrip(clone)
}

// withBearer wraps a transport with a token.
func withBearer(transport http.RoundTripper, token string) http.RoundTripper {
	return bearer{next: transport, token: token}
}

// cleartextHTTP2 is HTTP/2 without TLS, which is what gRPC over h2c and over a
// Unix socket speaks.
func cleartextHTTP2() *http.Protocols {
	protocols := new(http.Protocols)
	protocols.SetUnencryptedHTTP2(true)
	return protocols
}
