package link_test

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"os/user"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"connectrpc.com/connect"
	"github.com/coder/websocket"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/hosts"
	"github.com/aiviarium/rookery/internal/link"
	"github.com/aiviarium/rookery/internal/link/linktest"
	"github.com/aiviarium/rookery/internal/profile"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

type runtimeTest struct {
	address  string
	http     *http.Client
	ctx      context.Context
	link     *link.Manager
	database *store.Store
	account  string
}

func startRuntime(t *testing.T) runtimeTest {
	t.Helper()
	ctx := t.Context()
	opened := storetest.New(t)
	testSSH := linktest.Start(t, startDaemon(t))
	address, port := testSSH.Address()
	current, err := user.Current()
	if err != nil {
		t.Fatal(err)
	}
	desktop := current.Username
	account := "desktop"
	if desktop == "root" || desktop == "ops" {
		desktop = "dev"
		account = "operator"
		if current.Username == "root" {
			account = "root"
		}
	}
	hostStore := hosts.New(opened)
	record, err := hostStore.Add(ctx, hosts.AddInput{Name: "forge", Type: profile.TypeWorkstation, Profile: profile.Profile{
		Address: address, Port: port, Desktop: &profile.Desktop{User: desktop},
	}})
	if err != nil {
		t.Fatal(err)
	}
	links := manager(t, opened, link.Target{Name: "forge", Address: address, Port: port, SSHUser: "ops"})
	if _, err := links.Contact(ctx, link.Target{Name: "forge", Address: address, Port: port, SSHUser: "ops"}, link.Credential{Password: linktest.Password}, testLogger(t)); err != nil {
		t.Fatal(err)
	}
	checker := access.NewChecker(opened)
	if err := checker.GrantUser(ctx, "member", record.URI(), access.Use); err != nil {
		t.Fatal(err)
	}
	service := hosts.NewService(hosts.Deps{Hosts: hostStore, Checker: checker, Links: links, Log: testLogger(t)})
	runtime := hosts.NewRuntime(service)
	followCtx, cancel := context.WithCancel(ctx)
	done := make(chan struct{})
	go func() { defer close(done); _ = runtime.FollowChannels(followCtx) }()
	t.Cleanup(func() { cancel(); <-done })

	mux := http.NewServeMux()
	runtime.RegisterRoutes(mux)
	opts := connect.WithInterceptors(access.NewInterceptor(checker))
	handle := func(path string, handler http.Handler) { mux.Handle(path, handler) }
	handle(rookeryv1connect.NewTerminalServiceHandler(&hosts.TerminalService{Runtime: runtime}, opts))
	handle(rookeryv1connect.NewExecServiceHandler(&hosts.ExecService{Runtime: runtime}, opts))
	handle(rookeryv1connect.NewFileServiceHandler(&hosts.FileService{Runtime: runtime}, opts))
	handle(rookeryv1connect.NewPortServiceHandler(&hosts.PortService{Runtime: runtime}, opts))
	handle(rookeryv1connect.NewChannelServiceHandler(&hosts.ChannelService{Runtime: runtime}, opts))
	handle(rookeryv1connect.NewHostCallServiceHandler(&hosts.HostCallService{Runtime: runtime}, opts))
	edge := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		ctx := r.Context()
		switch r.Header.Get("Authorization") {
		case "Bearer manage-test":
			ctx = access.WithPrincipal(ctx, access.LocalAdmin())
		case "Bearer use-test":
			ctx = access.WithPrincipal(ctx, access.Principal{Kind: access.KindUser, ID: "member"})
		}
		mux.ServeHTTP(w, r.WithContext(ctx))
	}))
	protocols := new(http.Protocols)
	protocols.SetHTTP1(true)
	protocols.SetUnencryptedHTTP2(true)
	edge.Config.Protocols = protocols
	edge.Start()
	t.Cleanup(edge.Close)
	h2c := new(http.Protocols)
	h2c.SetUnencryptedHTTP2(true)
	return runtimeTest{address: edge.URL, http: &http.Client{Transport: &http.Transport{Protocols: h2c}},
		ctx: ctx, link: links, database: opened, account: account}
}

func managedRequest[T any](message *T) *connect.Request[T] {
	request := connect.NewRequest(message)
	request.Header().Set("Authorization", "Bearer manage-test")
	return request
}

func TestHostExecAndFileOperationsAcrossSSH(t *testing.T) {
	t.Parallel()
	f := startRuntime(t)
	exec := rookeryv1connect.NewExecServiceClient(f.http, f.address, connect.WithGRPC())
	command := &rookeryv1.ExecServiceRunRequest{Host: "forge", Exec: &hostv1.ExecServiceRunRequest{
		User: &hostv1.User{Name: f.account}, Command: "/bin/sh", Args: []string{"-c", "printf out; printf err >&2; exit 7"},
	}}
	stream, err := exec.Run(f.ctx, managedRequest(command))
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = stream.Close() }()
	var stdout, stderr strings.Builder
	var exit int32
	for stream.Receive() {
		event := stream.Msg().GetEvent()
		stdout.Write(event.GetStdout())
		stderr.Write(event.GetStderr())
		if event.GetExit() != nil {
			exit = event.GetExit().GetCode()
		}
	}
	if err := stream.Err(); err != nil || stdout.String() != "out" || stderr.String() != "err" || exit != 7 {
		t.Fatalf("stdout %q stderr %q exit %d error %v", stdout.String(), stderr.String(), exit, err)
	}

	command.Exec.User.Name = "root"
	deniedRequest := connect.NewRequest(command)
	deniedRequest.Header().Set("Authorization", "Bearer use-test")
	deniedStream, err := exec.Run(f.ctx, deniedRequest)
	if err == nil {
		defer func() { _ = deniedStream.Close() }()
		_ = deniedStream.Receive()
		err = deniedStream.Err()
	}
	if connect.CodeOf(err) != connect.CodePermissionDenied {
		t.Fatalf("root command with use: %v", err)
	}

	files := rookeryv1connect.NewFileServiceClient(f.http, f.address, connect.WithGRPC())
	path := filepath.Join(t.TempDir(), "notes")
	account := &hostv1.User{Name: f.account}
	write := &fileWrite{host: "forge", path: path, user: account}
	if _, err := files.Write(f.ctx, managedRequest(write.request("first", ""))); err != nil {
		t.Fatal(err)
	}
	if _, err := files.Write(f.ctx, managedRequest(write.request("second", "sha256:stale"))); connect.CodeOf(err) != connect.CodeAborted {
		t.Fatalf("stale write: %v", err)
	}
	got, err := files.Read(f.ctx, managedRequest(&rookeryv1.FileServiceReadRequest{
		Host: "forge", File: &hostv1.FileServiceReadRequest{User: account, Path: path, Offset: 1, Length: 3},
	}))
	if err != nil || string(got.Msg.GetResult().GetData()) != "irs" {
		t.Fatalf("read = %v, %v", got, err)
	}
}

type fileWrite struct {
	host, path string
	user       *hostv1.User
}

func (f fileWrite) request(content, revision string) *rookeryv1.FileServiceWriteRequest {
	return &rookeryv1.FileServiceWriteRequest{Host: f.host, File: &hostv1.FileServiceWriteRequest{
		User: f.user, Path: f.path, Data: []byte(content), Revision: revision,
	}}
}

func TestHostTerminalWebSocketAndHalfClose(t *testing.T) {
	t.Parallel()
	f := startRuntime(t)
	ctx, cancel := context.WithTimeout(f.ctx, 5*time.Second)
	defer cancel()
	terminals := rookeryv1connect.NewTerminalServiceClient(f.http, f.address, connect.WithGRPC())
	opened, err := terminals.Open(ctx, managedRequest(&rookeryv1.TerminalServiceOpenRequest{
		Host: "forge", Session: &hostv1.SessionServiceCreateRequest{
			User: &hostv1.User{Name: f.account}, Command: "/bin/sh", Args: []string{"-c", "read line; stty size; printf 'echo: %s\n' \"$line\""},
		}, Private: true,
	}))
	if err != nil {
		t.Fatal(err)
	}
	id := opened.Msg.GetTerminal().GetSession().GetId()
	address := "ws" + strings.TrimPrefix(f.address, "http") + "/ws/terminals/" + id
	socket, response, err := websocket.Dial(ctx, address, &websocket.DialOptions{HTTPHeader: http.Header{
		"Authorization": {"Bearer use-test"},
	}})
	if socket != nil {
		_ = socket.CloseNow()
	}
	if response != nil && response.Body != nil {
		_ = response.Body.Close()
	}
	if err == nil || response == nil || response.StatusCode != http.StatusForbidden {
		t.Fatalf("attach to private terminal = response %v, %v", response, err)
	}
	socket, response, err = websocket.Dial(ctx, address, &websocket.DialOptions{HTTPHeader: http.Header{
		"Authorization": {"Bearer manage-test"},
	}})
	if response != nil && response.Body != nil {
		_ = response.Body.Close()
	}
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = socket.CloseNow() }()
	if err := socket.Write(ctx, websocket.MessageText, []byte(`{"type":"resize","cols":112,"rows":33}`)); err != nil {
		t.Fatal(err)
	}
	if err := socket.Write(ctx, websocket.MessageBinary, []byte("hello\n")); err != nil {
		t.Fatal(err)
	}
	var output strings.Builder
	for {
		kind, data, err := socket.Read(ctx)
		if err != nil {
			t.Fatal(err)
		}
		if kind == websocket.MessageText && string(data) == `{"type":"exit","code":0}` {
			break
		}
		output.Write(data)
	}
	if !strings.Contains(output.String(), "echo: hello") || !strings.Contains(output.String(), "33 112") {
		t.Fatalf("terminal output = %q", output.String())
	}
	_ = socket.Close(websocket.StatusNormalClosure, "")

	stream := terminals.Attach(ctx)
	stream.RequestHeader().Set("Authorization", "Bearer manage-test")
	if err := stream.Send(&rookeryv1.TerminalServiceAttachRequest{Attach: &hostv1.SessionServiceAttachRequest{Id: id}}); err != nil {
		t.Fatal(err)
	}
	if err := stream.CloseRequest(); err != nil {
		t.Fatal(err)
	}
	defer func() { _ = stream.CloseResponse() }()
	var replay strings.Builder
	for {
		message, err := stream.Receive()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			t.Fatal(err)
		}
		replay.Write(message.GetEvent().GetOutput())
	}
	if !strings.Contains(replay.String(), "echo: hello") {
		t.Fatalf("replayed %q", replay.String())
	}
}

func TestHostPortProxies(t *testing.T) {
	t.Parallel()
	f := startRuntime(t)
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "" || r.Header.Get("Cookie") != "" {
			t.Error("edge credentials leaked to the host")
		}
		w.Header().Set("Set-Cookie", "rookery_session=host-controlled")
		w.Header().Set("Content-Type", "text/plain")
		_, _ = io.WriteString(w, r.URL.RequestURI()) //nolint:gosec // the plain-text test backend echoes its input
	}))
	defer upstream.Close()
	port := strings.TrimPrefix(upstream.URL, "http://127.0.0.1:")
	request, err := http.NewRequestWithContext(f.ctx, http.MethodGet, f.address+"/hosts/forge/ports/"+port+"/test?q=one", nil)
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Authorization", "Bearer use-test")
	request.Header.Set("Cookie", "rookery_session=server-only")
	resp, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = resp.Body.Close() }()
	data, err := io.ReadAll(resp.Body)
	if err != nil || resp.StatusCode != http.StatusOK || string(data) != "/test?q=one" || resp.Header.Get("Set-Cookie") != "" {
		t.Fatalf("proxied response %d %q %v", resp.StatusCode, data, err)
	}

	ctx, cancel := context.WithTimeout(f.ctx, 5*time.Second)
	defer cancel()
	url := "ws" + strings.TrimPrefix(f.address, "http") + "/ws/hosts/forge/ports/" + port
	socket, response, err := websocket.Dial(ctx, url, &websocket.DialOptions{HTTPHeader: http.Header{
		"Authorization": {"Bearer use-test"},
	}})
	if response != nil && response.Body != nil {
		_ = response.Body.Close()
	}
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = socket.CloseNow() }()
	if err := socket.Write(ctx, websocket.MessageBinary, []byte("GET /socket HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")); err != nil {
		t.Fatal(err)
	}
	var wire strings.Builder
	for !strings.Contains(wire.String(), "\r\n\r\n/socket") {
		kind, data, err := socket.Read(ctx)
		if err != nil {
			t.Fatalf("websocket received %q: %v", wire.String(), err)
		}
		if kind != websocket.MessageBinary {
			t.Fatalf("received websocket kind %v", kind)
		}
		wire.Write(data)
	}
}

func TestHostChannelTakeIn(t *testing.T) {
	t.Parallel()
	f := startRuntime(t)
	channel := rookeryv1connect.NewChannelServiceClient(f.http, f.address, connect.WithGRPC())
	payload, err := cborx.Marshal("hello")
	if err != nil {
		t.Fatal(err)
	}
	response, err := channel.Publish(f.ctx, managedRequest(&rookeryv1.ChannelServicePublishRequest{
		Host: "forge", Publish: &hostv1.ChannelServicePublishRequest{
			Topic: "example/greeting", PayloadCbor: payload, Keep: hostv1.ChannelKeep_CHANNEL_KEEP_LOG,
		},
	}))
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(f.ctx, 5*time.Second)
	defer cancel()
	ticker := time.NewTicker(10 * time.Millisecond)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			t.Fatal("channel message did not arrive")
		case <-ticker.C:
		}
		messages, err := f.database.Queries().TopicMessagesForInspect(ctx, store.TopicMessagesForInspectParams{
			Topic: "host/forge/example/greeting", Limit: 10,
		})
		if err != nil {
			t.Fatal(err)
		}
		if len(messages) == 0 {
			continue
		}
		cursor, err := f.database.Queries().GetHostCursor(ctx, "forge")
		if err != nil || len(messages) != 1 || string(messages[0].Payload) != string(payload) || cursor.Sequence < 0 || uint64(cursor.Sequence) != response.Msg.GetMessage().GetSequence() {
			t.Fatalf("messages %v cursor %v error %v", messages, cursor, err)
		}
		return
	}
}

func TestHostCallRoutesToARegisteredProvider(t *testing.T) {
	t.Parallel()
	f := startRuntime(t)
	ctx, cancel := context.WithTimeout(f.ctx, 5*time.Second)
	defer cancel()

	upstream, release, err := f.link.Use(ctx, "forge")
	if err != nil {
		t.Fatal(err)
	}
	defer release()
	provider := upstream.HostCalls.Register(ctx)
	if err := provider.Send(&hostv1.HostCallServiceRegisterRequest{
		Message: &hostv1.HostCallServiceRegisterRequest_Service{Service: &hostv1.HostService{
			Name: "example/echo", Methods: []string{"echo"},
		}},
	}); err != nil {
		t.Fatal(err)
	}
	ack, err := provider.Receive()
	if err != nil || !ack.GetRegistered() {
		t.Fatalf("registration: %v %v", ack, err)
	}
	answered := make(chan error, 1)
	go func() {
		message, err := provider.Receive()
		if err != nil {
			answered <- err
			return
		}
		call := message.GetRequest()
		answered <- provider.Send(&hostv1.HostCallServiceRegisterRequest{
			Message: &hostv1.HostCallServiceRegisterRequest_Result{Result: &hostv1.HostCallResult{
				Id: call.GetId(), ResultCbor: call.GetArgsCbor(),
			}},
		})
	}()
	defer func() { cancel(); _ = provider.CloseRequest(); _ = provider.CloseResponse() }()

	edge := rookeryv1connect.NewHostCallServiceClient(f.http, f.address, connect.WithGRPC())
	ticker := time.NewTicker(10 * time.Millisecond)
	defer ticker.Stop()
	for {
		list, err := edge.List(ctx, managedRequest(&rookeryv1.HostCallServiceListRequest{Host: "forge"}))
		if err != nil {
			t.Fatal(err)
		}
		if len(list.Msg.GetServices()) > 0 {
			break
		}
		select {
		case <-ctx.Done():
			t.Fatal("provider did not register")
		case <-ticker.C:
		}
	}
	payload, err := cborx.Marshal("hello")
	if err != nil {
		t.Fatal(err)
	}
	response, err := edge.Call(ctx, managedRequest(&rookeryv1.HostCallServiceCallRequest{
		Host: "forge", Service: "example/echo", Method: "echo", ArgsCbor: payload,
	}))
	if err != nil || string(response.Msg.GetResultCbor()) != string(payload) {
		t.Fatalf("host call got %v: %v", response, err)
	}
	if err := <-answered; err != nil {
		t.Fatalf("provider: %v", err)
	}
	_, err = edge.Call(ctx, managedRequest(&rookeryv1.HostCallServiceCallRequest{
		Host: "forge", Service: "example/missing", Method: "echo",
	}))
	if connect.CodeOf(err) != connect.CodeUnavailable {
		t.Fatalf("calling an absent provider: %v", err)
	}
}
