package tailnet_test

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/aiviarium/rookery/internal/tailnet"
)

// tailscaleAPI is a fake Tailscale API, with the paths and bodies its v2 API has.
type tailscaleAPI struct {
	t      *testing.T
	server *httptest.Server
	// seen are the requests, as "METHOD /path", in order.
	seen []string
	// requests are the bodies the API was given, by path.
	bodies map[string]map[string]any
}

const tailscaleDevices = `{"devices":[
	{"id":"dev1","nodeId":"n1","hostname":"desk-1","name":"desk-1.example.ts.net",
	 "addresses":["100.64.0.7"],"tags":["tag:rookery"],"connectedToControl":true,
	 "lastSeen":"2026-01-02T03:04:05Z"},
	{"id":"dev2","nodeId":"n2","hostname":"server","name":"server.example.ts.net",
	 "addresses":["100.64.0.1"],"connectedToControl":true}
]}`

func newTailscaleAPI(t *testing.T) *tailscaleAPI {
	t.Helper()

	api := &tailscaleAPI{t: t, bodies: map[string]map[string]any{}}
	api.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		path := r.Method + " " + r.URL.Path
		api.seen = append(api.seen, path)

		if r.Body != nil {
			var body map[string]any
			if err := json.NewDecoder(r.Body).Decode(&body); err == nil {
				api.bodies[path] = body
			}
		}

		switch path {
		case "POST /api/v2/tailnet/-/keys":
			w.WriteHeader(http.StatusOK)
			_, _ = w.Write([]byte(`{"id":"k1","key":"tskey-auth-example","expires":"2026-02-03T04:05:06Z"}`))
		case "GET /api/v2/tailnet/-/devices":
			w.WriteHeader(http.StatusOK)
			_, _ = w.Write([]byte(tailscaleDevices))
		case "DELETE /api/v2/device/n1":
			w.WriteHeader(http.StatusOK)
		case "DELETE /api/v2/device/missing":
			w.WriteHeader(http.StatusNotFound)
			_, _ = w.Write([]byte(`{"message":"not found"}`))
		default:
			w.WriteHeader(http.StatusNotFound)
			_, _ = w.Write([]byte(`{"message":"not found"}`))
		}
	}))
	t.Cleanup(api.server.Close)
	return api
}

func (a *tailscaleAPI) client(t *testing.T) *tailnet.Tailscale {
	t.Helper()

	//nolint:gosec // a made-up key, for a fake API on a loopback address.
	client, err := tailnet.NewTailscale(tailnet.TailscaleOptions{
		APIKey:  "tskey-api-example",
		BaseURL: a.server.URL,
	})
	if err != nil {
		t.Fatalf("NewTailscale() = %v", err)
	}
	return client
}

func TestTailscaleMintKey(t *testing.T) {
	t.Parallel()

	api := newTailscaleAPI(t)
	client := api.client(t)

	key, err := client.MintKey(context.Background(), tailnet.KeyRequest{
		Hostname: "desk-1",
		Tags:     []string{"tag:rookery"},
		Expiry:   time.Hour,
	})
	if err != nil {
		t.Fatalf("MintKey() = %v", err)
	}
	if key.Secret != "tskey-auth-example" || key.ID != "k1" {
		t.Errorf("MintKey() = %+v", key)
	}

	body := api.bodies["POST /api/v2/tailnet/-/keys"]
	capabilities, _ := body["capabilities"].(map[string]any)
	devices, _ := capabilities["devices"].(map[string]any)
	create, _ := devices["create"].(map[string]any)
	if create["reusable"] != false && create["reusable"] != nil {
		t.Errorf("the key is reusable: %v", create)
	}
	if create["preauthorized"] != true || create["ephemeral"] != false {
		t.Errorf("capabilities = %v", create)
	}
	tags, _ := create["tags"].([]any)
	if len(tags) != 1 || tags[0] != "tag:rookery" {
		t.Errorf("tags = %v", create["tags"])
	}
	if got, _ := body["expirySeconds"].(float64); int64(got) != 3600 {
		t.Errorf("expirySeconds = %v, want 3600", body["expirySeconds"])
	}
}

func TestTailscaleDevice(t *testing.T) {
	t.Parallel()

	api := newTailscaleAPI(t)
	client := api.client(t)

	device, err := client.Device(context.Background(), "desk-1")
	if err != nil {
		t.Fatalf("Device() = %v", err)
	}
	if device.ID != "n1" || device.Hostname != "desk-1" {
		t.Errorf("Device() = %+v, want the node id and the hostname", device)
	}
	if got, want := device.Address(), "100.64.0.7"; got != want {
		t.Errorf("Address() = %q, want %q", got, want)
	}

	// The tailnet's domain is not part of a host's name.
	if _, err := client.Device(context.Background(), "Server"); err != nil {
		t.Errorf("Device(Server) = %v, want the device with that name", err)
	}
	if _, err := client.Device(context.Background(), "nowhere"); !isNotFound(err) {
		t.Errorf("Device(nowhere) = %v, want a missing device", err)
	}
}

func TestTailscaleDevices(t *testing.T) {
	t.Parallel()

	api := newTailscaleAPI(t)
	client := api.client(t)

	devices, err := client.Devices(context.Background())
	if err != nil {
		t.Fatalf("Devices() = %v", err)
	}
	if len(devices) != 1 || devices[0].Hostname != "desk-1" {
		t.Errorf("Devices() = %+v, want only the tagged device", devices)
	}
}

func TestTailscaleDeleteDevice(t *testing.T) {
	t.Parallel()

	api := newTailscaleAPI(t)
	client := api.client(t)

	if err := client.DeleteDevice(context.Background(), "n1"); err != nil {
		t.Fatalf("DeleteDevice() = %v", err)
	}
	if got := api.seen[0]; got != "DELETE /api/v2/device/n1" {
		t.Errorf("the API saw %q", got)
	}
	if err := client.DeleteDevice(context.Background(), "missing"); !isNotFound(err) {
		t.Errorf("DeleteDevice(missing) = %v, want a missing device", err)
	}
}

func TestNewTailscaleRequiresItsKey(t *testing.T) {
	t.Parallel()

	if _, err := tailnet.NewTailscale(tailnet.TailscaleOptions{}); err == nil {
		t.Error("NewTailscale() accepted a client without a key")
	}
}
