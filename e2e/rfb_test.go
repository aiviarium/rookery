//go:build e2e

package e2e_test

import (
	"bufio"
	"bytes"
	"context"
	"errors"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"

	"github.com/aiviarium/rookery/internal/cli"
)

const rfbVersion = "RFB 003.008\n"
const rfbSecurity = "\x01\x01"

func serveRFB(ctx context.Context, t *testing.T, runtime string) func() error {
	t.Helper()
	dir := filepath.Join(runtime, "rookery")
	if err := os.Mkdir(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	listener, err := (&net.ListenConfig{}).Listen(ctx, "unix", filepath.Join(dir, "vnc.sock"))
	if err != nil {
		t.Fatal(err)
	}
	done := make(chan struct{})
	var failure error
	go func() {
		defer close(done)
		for range 2 {
			conn, err := listener.Accept()
			if err != nil {
				failure = err
				return
			}
			if err := readRFB(conn); err != nil {
				failure = err
				return
			}
		}
	}()
	t.Cleanup(func() {
		_ = listener.Close()
		<-done
	})
	return func() error {
		<-done
		return failure
	}
}

func checkBrowserRFB(ctx context.Context, t *testing.T, site string, browser *http.Client) {
	t.Helper()
	address := "ws" + strings.TrimPrefix(site, "http") + "/ws/hosts/studio/desktop"
	conn, response, err := websocket.Dial(ctx, address, &websocket.DialOptions{
		HTTPClient:   browser,
		HTTPHeader:   http.Header{"Origin": {site}},
		Subprotocols: []string{"binary"},
	})
	if response != nil && response.Body != nil {
		_ = response.Body.Close()
	}
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = conn.CloseNow() }()
	if kind, data, err := conn.Read(ctx); err != nil || kind != websocket.MessageBinary || string(data) != rfbVersion {
		t.Fatalf("browser RFB greeting: %v %q %v", kind, data, err)
	}
	if err := conn.Write(ctx, websocket.MessageBinary, []byte(rfbVersion)); err != nil {
		t.Fatal(err)
	}
	if kind, data, err := conn.Read(ctx); err != nil || kind != websocket.MessageBinary || string(data) != rfbSecurity {
		t.Fatalf("browser RFB security: %v %q %v", kind, data, err)
	}
	if _, _, err := conn.Read(ctx); websocket.CloseStatus(err) != websocket.StatusNormalClosure {
		t.Fatalf("VNC shutdown in browser: %v", err)
	}
}

func checkCLIRFB(ctx context.Context, t *testing.T) {
	t.Helper()
	cmd := cli.New("e2e") //nolint:contextcheck // Cobra receives the context at ExecuteContext.
	cmd.SetArgs([]string{"desktop", "studio", "--local", "127.0.0.1:0"})
	read, write := io.Pipe()
	defer func() { _ = read.Close() }()
	cmd.SetOut(write)
	var diagnostics bytes.Buffer
	cmd.SetErr(&diagnostics)

	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	done := make(chan error, 1)
	go func() {
		defer func() { _ = write.Close() }()
		done <- cmd.ExecuteContext(ctx)
	}()
	defer func() {
		cancel()
		if err := <-done; err != nil {
			t.Errorf("rookery desktop: %v, %s", err, &diagnostics)
		}
	}()

	ready, err := bufio.NewReader(read).ReadString('\n')
	if err != nil {
		t.Fatalf("reading the CLI's VNC listen address: %v", err)
	}
	conn, err := (&net.Dialer{}).DialContext(ctx, "tcp", strings.TrimSpace(ready))
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = conn.Close() }()
	if err := conn.SetDeadline(time.Now().Add(4 * time.Second)); err != nil {
		t.Fatal(err)
	}
	greeting := make([]byte, len(rfbVersion))
	if _, err := io.ReadFull(conn, greeting); err != nil || string(greeting) != rfbVersion {
		t.Fatalf("CLI RFB greeting: %q: %v", greeting, err)
	}
	if _, err := io.WriteString(conn, rfbVersion); err != nil {
		t.Fatal(err)
	}
	security := make([]byte, len(rfbSecurity))
	if _, err := io.ReadFull(conn, security); err != nil || string(security) != rfbSecurity {
		t.Fatalf("CLI RFB security: %q: %v", security, err)
	}
	if _, err := conn.Read(make([]byte, 1)); !errors.Is(err, io.EOF) {
		t.Fatalf("VNC shutdown in CLI: %v", err)
	}
}
