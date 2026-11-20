package tailnet_test

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/aiviarium/rookery/internal/tailnet"
)

// headscale is a fake Headscale control server, with the paths and bodies its v1
// API has.
type headscale struct {
	t        *testing.T
	server   *httptest.Server
	requests []headscaleRequest
	// answer is what a path answers with; the status is the second value.
	answer map[string]struct {
		status int
		body   string
	}
	key string
}

type headscaleRequest struct {
	Method string
	Path   string
	Auth   string
	Body   map[string]any
}

const nodeAnswer = `{"nodes":[
	{"id":"1","name":"desk-1.example.ts.net","givenName":"desk-1",
	 "ipAddresses":["fd7a:115c:a1e0::1","100.64.0.7"],"online":true,
	 "lastSeen":"2026-01-02T03:04:05Z","forcedTags":["tag:rookery"]},
	{"id":"2","name":"server.example.ts.net","givenName":"server",
	 "ipAddresses":["100.64.0.1"],"online":true,"lastSeen":"2026-01-02T03:04:05Z"},
	{"id":"3","name":"lab.example.ts.net","givenName":"lab",
	 "ipAddresses":["100.64.0.9"],"online":false,"validTags":["tag:lab"]}
]}`

func newHeadscale(t *testing.T, answer map[string]struct {
	status int
	body   string
}) *headscale {
	t.Helper()

	fake := &headscale{
		t:      t,
		answer: answer,
		key:    "hskey-api-example",
	}
	fake.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		request := headscaleRequest{
			Method: r.Method,
			Path:   r.URL.Path,
			Auth:   r.Header.Get("Authorization"),
		}
		if r.Body != nil {
			var body map[string]any
			if err := json.NewDecoder(r.Body).Decode(&body); err == nil {
				request.Body = body
			}
		}
		fake.requests = append(fake.requests, request)

		path := r.Method + " " + r.URL.Path
		a, ok := fake.answer[path]
		if !ok {
			w.WriteHeader(http.StatusNotFound)
			_, _ = w.Write([]byte(`{"message":"not found"}`))
			return
		}
		w.WriteHeader(a.status)
		_, _ = w.Write([]byte(a.body))
	}))
	t.Cleanup(fake.server.Close)
	return fake
}

func (h *headscale) client(t *testing.T, options tailnet.HeadscaleOptions) *tailnet.Headscale {
	t.Helper()

	if options.URL == "" {
		options.URL = h.server.URL
	}
	if options.APIKey == "" {
		options.APIKey = h.key
	}
	client, err := tailnet.NewHeadscale(options)
	if err != nil {
		t.Fatalf("NewHeadscale() = %v", err)
	}
	return client
}

var headscaleAnswers = map[string]struct {
	status int
	body   string
}{
	"POST /api/v1/preauthkey": {
		status: http.StatusOK,
		body:   `{"preAuthKey":{"id":"7","key":"hskey-auth-example","expiration":"2026-02-03T04:05:06Z"}}`,
	},
	"GET /api/v1/node":       {status: http.StatusOK, body: nodeAnswer},
	"DELETE /api/v1/node/1":  {status: http.StatusOK, body: `{}`},
	"DELETE /api/v1/node/99": {status: http.StatusNotFound, body: `{"message":"node not found"}`},
}

func TestHeadscaleMintKey(t *testing.T) {
	t.Parallel()

	fake := newHeadscale(t, headscaleAnswers)
	client := fake.client(t, tailnet.HeadscaleOptions{User: "3"})

	key, err := client.MintKey(context.Background(), tailnet.KeyRequest{
		Hostname: "desk-1",
		Tags:     []string{"tag:rookery"},
	})
	if err != nil {
		t.Fatalf("MintKey() = %v", err)
	}
	if key.Secret != "hskey-auth-example" || key.ID != "7" {
		t.Errorf("MintKey() = %+v", key)
	}
	if len(fake.requests) != 1 {
		t.Fatalf("the control server saw %d requests, want 1", len(fake.requests))
	}
	request := fake.requests[0]
	if request.Auth != "Bearer hskey-api-example" {
		t.Errorf("Authorization = %q", request.Auth)
	}
	if request.Body["user"] != "3" {
		t.Errorf("user = %v, want the configured user", request.Body["user"])
	}
	if request.Body["reusable"] != false || request.Body["ephemeral"] != false {
		t.Errorf("the key is reusable or ephemeral: %v", request.Body)
	}
	tags, _ := request.Body["aclTags"].([]any)
	if len(tags) != 1 || tags[0] != "tag:rookery" {
		t.Errorf("aclTags = %v", request.Body["aclTags"])
	}
	if _, ok := request.Body["expiration"].(string); !ok {
		t.Errorf("expiration = %v, want a time", request.Body["expiration"])
	}
}

func TestHeadscaleDevice(t *testing.T) {
	t.Parallel()

	fake := newHeadscale(t, headscaleAnswers)
	client := fake.client(t, tailnet.HeadscaleOptions{User: "3"})

	device, err := client.Device(context.Background(), "desk-1")
	if err != nil {
		t.Fatalf("Device() = %v", err)
	}
	if device.ID != "1" || device.Hostname != "desk-1" {
		t.Errorf("Device() = %+v", device)
	}
	// The IPv4 address comes first, since it is the one links dial.
	if got, want := device.Address(), "100.64.0.7"; got != want {
		t.Errorf("Address() = %q, want %q", got, want)
	}
	if !device.Online || !device.Tagged([]string{"tag:rookery"}) {
		t.Errorf("Device() = %+v, want an online tagged device", device)
	}

	if _, err := client.Device(context.Background(), "nowhere"); err == nil {
		t.Error("Device() found a device that is not there")
	}
}

func TestHeadscaleDevices(t *testing.T) {
	t.Parallel()

	fake := newHeadscale(t, headscaleAnswers)
	client := fake.client(t, tailnet.HeadscaleOptions{User: "3"})

	devices, err := client.Devices(context.Background())
	if err != nil {
		t.Fatalf("Devices() = %v", err)
	}
	if len(devices) != 2 {
		t.Fatalf("Devices() = %+v, want the two tagged devices", devices)
	}
	for _, device := range devices {
		if device.Hostname == "server" {
			t.Errorf("Devices() listed an untagged device: %+v", devices)
		}
	}
}

func TestHeadscaleDeleteDevice(t *testing.T) {
	t.Parallel()

	fake := newHeadscale(t, headscaleAnswers)
	client := fake.client(t, tailnet.HeadscaleOptions{User: "3"})

	if err := client.DeleteDevice(context.Background(), "1"); err != nil {
		t.Fatalf("DeleteDevice() = %v", err)
	}
	if got := fake.requests[0].Method + " " + fake.requests[0].Path; got != "DELETE /api/v1/node/1" {
		t.Errorf("the control server saw %q", got)
	}
}

func TestHeadscaleRefuses(t *testing.T) {
	t.Parallel()

	answers := map[string]struct {
		status int
		body   string
	}{
		"DELETE /api/v1/node/99": {status: http.StatusNotFound, body: `{"message":"node not found"}`},
		"GET /api/v1/node":       {status: http.StatusUnauthorized, body: `{"message":"bad key"}`},
	}
	fake := newHeadscale(t, answers)
	client := fake.client(t, tailnet.HeadscaleOptions{User: "3"})

	if err := client.DeleteDevice(context.Background(), "99"); !isNotFound(err) {
		t.Errorf("DeleteDevice() = %v, want a missing device", err)
	}
	if _, err := client.Devices(context.Background()); !isRefused(err) {
		t.Errorf("Devices() = %v, want a refusal", err)
	}
}

func TestNewHeadscaleRequiresItsOptions(t *testing.T) {
	t.Parallel()

	if _, err := tailnet.NewHeadscale(tailnet.HeadscaleOptions{APIKey: "k"}); err == nil {
		t.Error("NewHeadscale() accepted a client without an address")
	}
	if _, err := tailnet.NewHeadscale(tailnet.HeadscaleOptions{URL: "https://headscale.example"}); err == nil {
		t.Error("NewHeadscale() accepted a client without a key")
	}
}

func TestDeviceAddressAndTags(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name    string
		device  tailnet.Device
		address string
		tagged  bool
		wantTag string
	}{
		{
			name:    "IPv4 first",
			device:  tailnet.Device{Addresses: []string{"fd7a::1", "100.64.0.7"}},
			address: "100.64.0.7",
		},
		{
			name:    "IPv6 only",
			device:  tailnet.Device{Addresses: []string{"fd7a::1"}},
			address: "fd7a::1",
		},
		{
			name:    "no address",
			device:  tailnet.Device{},
			address: "",
		},
		{
			name:    "tagged",
			device:  tailnet.Device{Tags: []string{"tag:rookery"}},
			tagged:  true,
			wantTag: "tag:rookery",
		},
		{
			name:   "another tag",
			device: tailnet.Device{Tags: []string{"tag:lab"}},
			tagged: false,
			// The device carries a tag, but not the one asked about.
			wantTag: "tag:rookery",
		},
	}

	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			if got := c.device.Address(); got != c.address {
				t.Errorf("Address() = %q, want %q", got, c.address)
			}
			if got := c.device.Tagged([]string{c.wantTag}); got != c.tagged {
				t.Errorf("Tagged() = %v, want %v", got, c.tagged)
			}
		})
	}
}

// isNotFound reports whether the error says the control server has no such
// thing.
func isNotFound(err error) bool { return err != nil && strings.Contains(err.Error(), "not found") }

// isRefused reports whether the error says the control server refused.
func isRefused(err error) bool { return err != nil && strings.Contains(err.Error(), "refused") }
