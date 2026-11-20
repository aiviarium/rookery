package tailnet

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"
)

// HeadscaleOptions configure a Headscale control server.
type HeadscaleOptions struct {
	// URL is the control server's address, such as https://headscale.example.
	URL string
	// APIKey is a key from `headscale apikeys create`.
	APIKey string
	// User is the Headscale user the hosts belong to: its name or its numeric
	// id. Empty uses the first user Headscale lists.
	User string
	// HTTP overrides the client, for a test.
	HTTP *http.Client
}

// Headscale is Control for a self-hosted Headscale, through its v1 HTTP API.
//
// Headscale's API is small and its versions disagree on details; this client
// speaks the v1 paths and reads the fields every version since 0.23 has, and
// treats a field it does not know as absent.
type Headscale struct {
	baseURL string
	apiKey  string
	user    string
	http    *http.Client
}

// NewHeadscale makes a Headscale control server client.
func NewHeadscale(options HeadscaleOptions) (*Headscale, error) {
	if options.URL == "" {
		return nil, fmt.Errorf("%w: Headscale needs the control server's address", ErrRefused)
	}
	if options.APIKey == "" {
		return nil, fmt.Errorf("%w: Headscale needs an API key", ErrRefused)
	}
	client := options.HTTP
	if client == nil {
		client = &http.Client{Timeout: 30 * time.Second}
	}
	return &Headscale{
		baseURL: strings.TrimSuffix(options.URL, "/"),
		apiKey:  options.APIKey,
		user:    options.User,
		http:    client,
	}, nil
}

// Error is a control server's refusal, with the status it answered with.
type Error struct {
	// Status is the HTTP status.
	Status int
	// Message is what the server said, on one line.
	Message string
}

func (e *Error) Error() string {
	return fmt.Sprintf("tailnet: the control server answered %d: %s", e.Status, e.Message)
}

// headscalePreAuthKey is Headscale's one-use key.
type headscalePreAuthKey struct {
	ID         string    `json:"id"`
	Key        string    `json:"key"`
	Expiration time.Time `json:"expiration"`
}

type headscaleKeyResponse struct {
	PreAuthKey headscalePreAuthKey `json:"preAuthKey"`
}

// headscaleNode is one device.
type headscaleNode struct {
	ID          string    `json:"id"`
	Name        string    `json:"name"`
	GivenName   string    `json:"givenName"`
	IPAddresses []string  `json:"ipAddresses"`
	Online      bool      `json:"online"`
	LastSeen    time.Time `json:"lastSeen"`
	ForcedTags  []string  `json:"forcedTags"`
	ValidTags   []string  `json:"validTags"`
}

type headscaleNodesResponse struct {
	Nodes []headscaleNode `json:"nodes"`
}

// MintKey makes a one-use, pre-authorized key for a host.
func (h *Headscale) MintKey(ctx context.Context, request KeyRequest) (Key, error) {
	if request.Hostname == "" {
		return Key{}, fmt.Errorf("%w: a key needs the host's name", ErrRefused)
	}
	expiry := request.Expiry
	if expiry <= 0 {
		expiry = DefaultKeyExpiry
	}
	user, err := h.userID(ctx)
	if err != nil {
		return Key{}, err
	}
	body := map[string]any{
		"user":       user,
		"reusable":   false,
		"ephemeral":  request.Ephemeral,
		"expiration": time.Now().UTC().Add(expiry).Format(time.RFC3339),
		"aclTags":    request.Tags,
	}
	var answer headscaleKeyResponse
	if err := h.call(ctx, http.MethodPost, "/api/v1/preauthkey", body, &answer); err != nil {
		return Key{}, err
	}
	return Key{
		ID:      answer.PreAuthKey.ID,
		Secret:  answer.PreAuthKey.Key,
		Expires: answer.PreAuthKey.Expiration,
	}, nil
}

// Device looks a device up by the hostname it joined with.
func (h *Headscale) Device(ctx context.Context, hostname string) (Device, error) {
	nodes, err := h.nodes(ctx)
	if err != nil {
		return Device{}, err
	}
	for _, node := range nodes {
		if matchesHostname(node.GivenName, node.Name, hostname) {
			return fromHeadscale(node), nil
		}
	}
	return Device{}, fmt.Errorf("%w: no device named %q", ErrNotFound, hostname)
}

// DeleteDevice removes a device from the tailnet.
func (h *Headscale) DeleteDevice(ctx context.Context, id string) error {
	if id == "" {
		return fmt.Errorf("%w: a device needs an id to be removed", ErrRefused)
	}
	return h.call(ctx, http.MethodDelete, "/api/v1/node/"+url.PathEscape(id), nil, nil)
}

// Devices lists the tailnet's tagged devices.
func (h *Headscale) Devices(ctx context.Context) ([]Device, error) {
	nodes, err := h.nodes(ctx)
	if err != nil {
		return nil, err
	}
	tagged := make([]Device, 0, len(nodes))
	for _, node := range nodes {
		if len(node.ForcedTags) > 0 || len(node.ValidTags) > 0 {
			tagged = append(tagged, fromHeadscale(node))
		}
	}
	return tagged, nil
}

// nodes is every device the control server knows.
func (h *Headscale) nodes(ctx context.Context) ([]headscaleNode, error) {
	var answer headscaleNodesResponse
	if err := h.call(ctx, http.MethodGet, "/api/v1/node", nil, &answer); err != nil {
		return nil, err
	}
	return answer.Nodes, nil
}

// userID is the Headscale user the hosts belong to: the one it was configured
// with, or the first it lists.
func (h *Headscale) userID(ctx context.Context) (string, error) {
	if h.user != "" {
		return h.user, nil
	}
	var answer struct {
		Users []struct {
			ID   string `json:"id"`
			Name string `json:"name"`
		} `json:"users"`
	}
	if err := h.call(ctx, http.MethodGet, "/api/v1/user", nil, &answer); err != nil {
		return "", err
	}
	if len(answer.Users) == 0 {
		return "", fmt.Errorf("%w: Headscale has no user for the hosts", ErrRefused)
	}
	return answer.Users[0].ID, nil
}

// call makes one request and decodes its answer into out, when out is not nil.
func (h *Headscale) call(ctx context.Context, method, path string, body, out any) error {
	var reader io.Reader
	if body != nil {
		encoded, err := json.Marshal(body)
		if err != nil {
			return fmt.Errorf("tailnet: encoding the request: %w", err)
		}
		reader = bytes.NewReader(encoded)
	}
	request, err := http.NewRequestWithContext(ctx, method, h.baseURL+path, reader)
	if err != nil {
		return fmt.Errorf("tailnet: building the request: %w", err)
	}
	request.Header.Set("Authorization", "Bearer "+h.apiKey)
	request.Header.Set("Accept", "application/json")
	if body != nil {
		request.Header.Set("Content-Type", "application/json")
	}

	response, err := h.http.Do(request)
	if err != nil {
		return fmt.Errorf("%w: %w", ErrUnreachable, err)
	}
	defer func() { _ = response.Body.Close() }()

	// A refusal's body is small and says why; cap it whatever it is.
	answer, err := io.ReadAll(io.LimitReader(response.Body, 1<<20))
	if err != nil {
		return fmt.Errorf("%w: reading the answer: %w", ErrUnreachable, err)
	}
	if response.StatusCode < 200 || response.StatusCode > 299 {
		refusal := &Error{Status: response.StatusCode, Message: oneLine(answer)}
		switch response.StatusCode {
		case http.StatusNotFound:
			return fmt.Errorf("%w: %w", ErrNotFound, refusal)
		case http.StatusUnauthorized, http.StatusForbidden:
			return fmt.Errorf("%w: %w", ErrRefused, refusal)
		}
		return refusal
	}
	if out == nil || len(bytes.TrimSpace(answer)) == 0 {
		return nil
	}
	if err := json.Unmarshal(answer, out); err != nil {
		return fmt.Errorf("tailnet: reading the control server's answer: %w", err)
	}
	return nil
}

// fromHeadscale is a device in our own terms.
func fromHeadscale(node headscaleNode) Device {
	device := Device{
		ID:        node.ID,
		Hostname:  node.GivenName,
		Addresses: node.IPAddresses,
		Tags:      append(append([]string(nil), node.ForcedTags...), node.ValidTags...),
		Online:    node.Online,
		LastSeen:  node.LastSeen,
	}
	if device.Hostname == "" {
		device.Hostname, _, _ = strings.Cut(node.Name, ".")
	}
	// The v4 address first, which is what a link dials.
	for i, address := range device.Addresses {
		if strings.Count(address, ".") == 3 && i != 0 {
			copy(device.Addresses[1:i+1], device.Addresses[0:i])
			device.Addresses[0] = address
			break
		}
	}
	return device
}

// oneLine is a response body as one line of at most a few hundred bytes, for an
// error message.
func oneLine(body []byte) string {
	line := strings.TrimSpace(string(body))
	line = strings.Join(strings.Fields(line), " ")
	if len(line) > 300 {
		line = line[:300] + "…"
	}
	if line == "" {
		return "no message"
	}
	return line
}
