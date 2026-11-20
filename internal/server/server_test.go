package server_test

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/aiviarium/rookery/internal/server"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// freeAddress is a port nothing listens on, for a test's server.
func freeAddress(t *testing.T) string {
	t.Helper()

	var listenConfig net.ListenConfig
	listener, err := listenConfig.Listen(t.Context(), "tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("finding a free port: %v", err)
	}
	address := listener.Addr().String()
	if err := listener.Close(); err != nil {
		t.Fatalf("closing the port: %v", err)
	}
	return address
}

// start runs a server for a configuration, and returns once it is up.
func start(t *testing.T, config server.Config) (health func() (int, string), stop func()) {
	t.Helper()

	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan error, 1)
	built := server.New(config, slog.New(slog.DiscardHandler))
	go func() {
		err := built.Run(ctx)
		if err != nil {
			t.Logf("the server stopped: %v", err)
		}
		done <- err
	}()

	health = func() (int, string) {
		client := &http.Client{Timeout: time.Second}
		request, err := http.NewRequestWithContext(context.Background(), http.MethodGet, "http://"+config.Listen+"/healthz", nil)
		if err != nil {
			return 0, err.Error()
		}
		response, err := client.Do(request)
		if err != nil {
			return 0, err.Error()
		}
		defer func() { _ = response.Body.Close() }()
		body, _ := io.ReadAll(io.LimitReader(response.Body, 1024))
		return response.StatusCode, string(body)
	}

	var once sync.Once
	stop = func() {
		once.Do(func() {
			cancel()
			select {
			case err := <-done:
				_ = err
			case <-time.After(10 * time.Second):
				t.Error("the server did not stop")
			}
		})
	}
	t.Cleanup(stop)
	return health, stop
}

// waitFor waits until a server answers its health check with a code.
func waitFor(t *testing.T, health func() (int, string), want int) string {
	t.Helper()

	deadline := time.Now().Add(15 * time.Second)
	var body string
	for time.Now().Before(deadline) {
		code, got := health()
		if code == want {
			return got
		}
		body = fmt.Sprintf("%d %s", code, got)
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("no server answered %d: the last answer was %s", want, body)
	return ""
}

func TestOneServerLeadsAndAStandbyTakesOver(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	state := t.TempDir()

	first := server.Config{
		DatabaseURL:     opened.URL(),
		DatabaseLockURL: opened.URL(),
		Listen:          freeAddress(t),
		Socket:          filepath.Join(state, "first.sock"),
		SocketMode:      0o660,
		PublicURL:       "http://127.0.0.1",
		StateDir:        state,
		SecretKeyFile:   filepath.Join(state, "secret.key"),
		RetentionDays:   90,
		CookieSecure:    false,
		LogLevel:        slog.LevelError,
	}
	second := first
	second.Listen = freeAddress(t)
	second.Socket = filepath.Join(state, "second.sock")

	firstHealth, stopFirst := start(t, first)
	waitFor(t, firstHealth, http.StatusOK)

	secondHealth, _ := start(t, second)
	waitFor(t, secondHealth, http.StatusServiceUnavailable)
	if code, _ := firstHealth(); code != http.StatusOK {
		t.Fatalf("the leader answered %d once a standby was there", code)
	}

	// The standby takes over when the leader stops.
	stopFirst()
	waitFor(t, secondHealth, http.StatusOK)
}

func TestTheUnixSocketServesTheLocalAdmin(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	state := t.TempDir()
	socket := filepath.Join(state, "rookery.sock")

	config := server.Config{
		DatabaseURL:     opened.URL(),
		DatabaseLockURL: opened.URL(),
		Listen:          freeAddress(t),
		Socket:          socket,
		SocketMode:      0o660,
		PublicURL:       "http://127.0.0.1",
		StateDir:        state,
		SecretKeyFile:   filepath.Join(state, "secret.key"),
		RetentionDays:   90,
		CookieSecure:    false,
		LogLevel:        slog.LevelError,
	}
	health, _ := start(t, config)
	waitFor(t, health, http.StatusOK)

	// The socket answers, and only its group may use it.
	client := &http.Client{
		Transport: &http.Transport{
			DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
				var dialer net.Dialer
				return dialer.DialContext(ctx, "unix", socket)
			},
		},
		Timeout: 5 * time.Second,
	}
	request, err := http.NewRequestWithContext(t.Context(), http.MethodGet, "http://rookery/healthz", nil)
	if err != nil {
		t.Fatalf("building the call: %v", err)
	}
	response, err := client.Do(request)
	if err != nil {
		t.Fatalf("calling the socket: %v", err)
	}
	defer func() { _ = response.Body.Close() }()
	if response.StatusCode != http.StatusOK {
		t.Fatalf("the socket answered %d", response.StatusCode)
	}
}
