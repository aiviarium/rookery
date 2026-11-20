package desktop_test

import (
	"context"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"os/user"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"connectrpc.com/connect"
	"github.com/coder/websocket"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/agent"
	"github.com/aiviarium/rookery/internal/desktop"
	"github.com/aiviarium/rookery/internal/hosts"
	"github.com/aiviarium/rookery/internal/link"
	"github.com/aiviarium/rookery/internal/link/linktest"
	"github.com/aiviarium/rookery/internal/profile"
	"github.com/aiviarium/rookery/internal/seal"
	"github.com/aiviarium/rookery/internal/store/storetest"
	"github.com/aiviarium/rookery/internal/streamio"
)

func TestRFBOverGRPCAndWebSocket(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithTimeout(t.Context(), 8*time.Second)
	defer cancel()

	opened := storetest.New(t)
	checker := access.NewChecker(opened)
	if _, err := checker.Grant(ctx, access.HostURI("studio"), "everyone", access.View); err != nil {
		t.Fatal(err)
	}
	hostStore := hosts.New(opened)

	rfb, rootSocket := startDesktopHost(t)
	ssh := linktest.Start(t, rootSocket)
	host, port := ssh.Address()
	if _, err := hostStore.Add(ctx, hosts.AddInput{
		Name: "studio",
		Profile: profile.Profile{
			Address: host, Port: port, Type: profile.TypeWorkstation, Desktop: &profile.Desktop{},
		},
	}); err != nil {
		t.Fatal(err)
	}
	key, err := seal.New()
	if err != nil {
		t.Fatal(err)
	}
	ring, err := seal.NewRing(key.ID(), key)
	if err != nil {
		t.Fatal(err)
	}
	links := link.NewManager(opened, link.NewRing(opened, ring), link.NewKnownHosts(opened, nil), link.Options{
		Targets: link.TargetsFunc(func(context.Context, string) (link.Target, error) {
			return link.Target{Name: "studio", Address: host, Port: port, SSHUser: "ops"}, nil
		}),
	})
	t.Cleanup(links.Close)
	if _, err := links.Contact(ctx, link.Target{Name: "studio", Address: host, Port: port, SSHUser: "ops"}, link.Credential{Password: linktest.Password}, nil); err != nil {
		t.Fatal(err)
	}

	service := desktop.New(hostStore, links, checker)
	mux := http.NewServeMux()
	path, handler := rookeryv1connect.NewDesktopServiceHandler(service, connect.WithInterceptors(access.NewInterceptor(checker)))
	mux.Handle(path, streamio.WithReadInterrupt(handler))
	mux.Handle("GET /ws/hosts/{name}/desktop", service.WebSocket())
	httpServer := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var caller access.Principal
		switch r.Header.Get("Test-Caller") {
		case "admin":
			caller = access.LocalAdmin()
		case "viewer":
			caller = access.Principal{Kind: access.KindUser, ID: "viewer"}
		}
		mux.ServeHTTP(w, r.WithContext(access.WithPrincipal(r.Context(), caller)))
	}))
	protocols := new(http.Protocols)
	protocols.SetHTTP1(true)
	protocols.SetUnencryptedHTTP2(true)
	httpServer.Config.Protocols = protocols
	httpServer.Start()
	t.Cleanup(httpServer.Close)

	transport := &http.Transport{Protocols: new(http.Protocols)}
	transport.Protocols.SetUnencryptedHTTP2(true)
	t.Cleanup(transport.CloseIdleConnections)
	client := rookeryv1connect.NewDesktopServiceClient(&http.Client{Transport: transport}, httpServer.URL, connect.WithGRPC())
	refused := client.Connect(ctx)
	refused.RequestHeader().Set("Test-Caller", "viewer")
	_ = refused.Send(&rookeryv1.DesktopServiceConnectRequest{Host: "studio"})
	if _, err := refused.Receive(); connect.CodeOf(err) != connect.CodePermissionDenied {
		t.Fatalf("view-only gRPC: %v", err)
	}
	_ = refused.CloseRequest()
	_ = refused.CloseResponse()

	wsURL := "ws" + strings.TrimPrefix(httpServer.URL, "http") + "/ws/hosts/studio/desktop"
	_, response, err := websocket.Dial(ctx, wsURL, &websocket.DialOptions{HTTPHeader: http.Header{"Test-Caller": {"viewer"}}})
	if err == nil || response == nil || response.StatusCode != http.StatusForbidden {
		t.Fatalf("view-only WebSocket: %v, %v", response, err)
	}
	_ = response.Body.Close()

	const version = "RFB 003.008\n"
	const answer = "\x01\x01"
	rfbDone := make(chan error, 1)
	go func() {
		for range 2 {
			conn, err := rfb.Accept()
			if err != nil {
				rfbDone <- err
				return
			}
			err = rfbHandshake(conn, version, answer)
			if err != nil {
				rfbDone <- err
				return
			}
		}
		rfbDone <- nil
	}()
	defer func() {
		_ = rfb.Close()
		if err := <-rfbDone; err != nil && !errors.Is(err, net.ErrClosed) {
			t.Errorf("fake VNC: %v", err)
		}
	}()

	stream := client.Connect(ctx)
	stream.RequestHeader().Set("Test-Caller", "admin")
	if err := stream.Send(&rookeryv1.DesktopServiceConnectRequest{Host: "studio"}); err != nil {
		t.Fatal(err)
	}
	if hello, err := stream.Receive(); err != nil || string(hello.GetData()) != version {
		t.Fatalf("gRPC greeting: %q, %v", hello.GetData(), err)
	}
	if err := stream.Send(&rookeryv1.DesktopServiceConnectRequest{Data: []byte(version)}); err != nil {
		t.Fatal(err)
	}
	if result, err := stream.Receive(); err != nil || string(result.GetData()) != answer {
		t.Fatalf("gRPC answer: %q, %v", result.GetData(), err)
	}
	if _, err := stream.Receive(); !errors.Is(err, io.EOF) {
		t.Fatalf("desktop closed while the client was still sending: got %v, want EOF", err)
	}
	_ = stream.CloseRequest()
	_ = stream.CloseResponse()

	socket, response, err := websocket.Dial(ctx, wsURL, &websocket.DialOptions{
		Subprotocols: []string{"binary"},
		HTTPHeader:   http.Header{"Test-Caller": {"admin"}},
	})
	if response != nil && response.Body != nil {
		_ = response.Body.Close()
	}
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = socket.CloseNow() }()
	if kind, hello, err := socket.Read(ctx); err != nil || kind != websocket.MessageBinary || string(hello) != version {
		t.Fatalf("WebSocket greeting: %v %q, %v", kind, hello, err)
	}
	if err := socket.Write(ctx, websocket.MessageBinary, []byte(version)); err != nil {
		t.Fatal(err)
	}
	if kind, result, err := socket.Read(ctx); err != nil || kind != websocket.MessageBinary || string(result) != answer {
		t.Fatalf("WebSocket answer: %v %q, %v", kind, result, err)
	}
	if _, _, err := socket.Read(ctx); websocket.CloseStatus(err) != websocket.StatusNormalClosure {
		t.Fatalf("WebSocket desktop close: %v", err)
	}
}

func startDesktopHost(t *testing.T) (net.Listener, string) {
	t.Helper()
	dir := t.TempDir()
	runtime := filepath.Join(dir, "rookery")
	if err := os.Mkdir(runtime, 0o700); err != nil {
		t.Fatal(err)
	}
	listener, err := (&net.ListenConfig{}).Listen(t.Context(), "unix", filepath.Join(runtime, "vnc.sock"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })

	env := filepath.Join(dir, "desktop.env")
	if err := os.WriteFile(env, []byte("XDG_RUNTIME_DIR="+dir+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	current, err := user.Current()
	if err != nil {
		t.Fatal(err)
	}
	daemon, err := agent.New(t.Context(), agent.Config{
		DesktopEnvPath: env, DesktopUser: current.Username,
		RootSocket: filepath.Join(dir, "root.sock"), UserSocket: filepath.Join(dir, "user.sock"),
	})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan error, 1)
	go func() { done <- daemon.Serve(ctx) }()
	t.Cleanup(func() {
		cancel()
		if err := <-done; err != nil {
			t.Error(err)
		}
	})
	return listener, daemon.RootSocket()
}

func rfbHandshake(conn net.Conn, version, answer string) error {
	defer func() { _ = conn.Close() }()
	if err := conn.SetDeadline(time.Now().Add(3 * time.Second)); err != nil {
		return err
	}
	if _, err := io.WriteString(conn, version); err != nil {
		return err
	}
	received := make([]byte, len(version))
	if _, err := io.ReadFull(conn, received); err != nil {
		return err
	}
	if string(received) != version {
		return errors.New("the viewer returned the wrong RFB version")
	}
	_, err := io.WriteString(conn, answer)
	return err
}
