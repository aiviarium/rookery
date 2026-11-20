package docs

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/data"
)

// principalInterceptor sends the test's caller as a header on every call.
type principalInterceptor struct {
	principal data.Principal
}

// WrapUnary sets the header.
func (i principalInterceptor) WrapUnary(next connect.UnaryFunc) connect.UnaryFunc {
	return func(ctx context.Context, request connect.AnyRequest) (connect.AnyResponse, error) {
		request.Header().Set("X-Test-Principal", i.principal.String())
		return next(ctx, request)
	}
}

// WrapStreamingClient sets the header.
func (i principalInterceptor) WrapStreamingClient(next connect.StreamingClientFunc) connect.StreamingClientFunc {
	return func(ctx context.Context, spec connect.Spec) connect.StreamingClientConn {
		connection := next(ctx, spec)
		connection.RequestHeader().Set("X-Test-Principal", i.principal.String())
		return connection
	}
}

// WrapStreamingHandler does nothing: this is a client interceptor.
func (i principalInterceptor) WrapStreamingHandler(next connect.StreamingHandlerFunc) connect.StreamingHandlerFunc {
	return next
}

// callerFromHeader reads the caller the middleware put in the context.
type callerFromHeader struct{}

// Principal is the caller.
func (callerFromHeader) Principal(ctx context.Context) (data.Principal, error) {
	principal, ok := ctx.Value(principalKey{}).(data.Principal)
	if !ok {
		return data.Principal{}, errors.New("docs: no caller")
	}
	return principal, nil
}

// principalKey is where the middleware puts the caller.
type principalKey struct{}

// serviceClient returns a client for the document service, acting as a
// principal.
func serviceClient(t *testing.T, d *documents, principal data.Principal) rookeryv1connect.DocumentServiceClient {
	t.Helper()

	path, handler := rookeryv1connect.NewDocumentServiceHandler(NewService(d.Documents, callerFromHeader{}))
	mux := http.NewServeMux()
	mux.Handle(path, http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		caller := principal
		if header := request.Header.Get("X-Test-Principal"); header != "" {
			kind, id, _ := cut(header, ":")
			caller = data.Principal{Kind: data.PrincipalKind(kind), ID: id}
		}
		handler.ServeHTTP(writer, request.WithContext(context.WithValue(request.Context(), principalKey{}, caller)))
	}))

	// The bidi stream needs HTTP/2, which a test server speaks once it is TLS.
	server := httptest.NewUnstartedServer(mux)
	server.EnableHTTP2 = true
	server.StartTLS()
	t.Cleanup(server.Close)

	// The server's own client does not ask for HTTP/2 when its transport has a
	// TLS configuration, and a bidi stream needs it.
	transport := server.Client().Transport.(*http.Transport).Clone()
	protocols := new(http.Protocols)
	protocols.SetHTTP2(true)
	transport.Protocols = protocols
	client := &http.Client{Transport: transport}
	t.Cleanup(transport.CloseIdleConnections)

	return rookeryv1connect.NewDocumentServiceClient(client, server.URL,
		connect.WithGRPC(), connect.WithInterceptors(principalInterceptor{principal: principal}))
}

// cut is strings.Cut.
func cut(text, separator string) (before, after string, found bool) {
	for i := 0; i+len(separator) <= len(text); i++ {
		if text[i:i+len(separator)] == separator {
			return text[:i], text[i+len(separator):], true
		}
	}
	return text, "", false
}

// TestService checks the unary document API over a connect client.
func TestService(t *testing.T) {
	t.Parallel()

	d := newDocuments(t)
	client := serviceClient(t, d, d.user)
	ctx := t.Context()

	created, err := client.Create(ctx, connect.NewRequest(&rookeryv1.DocumentServiceCreateRequest{
		Type: "yjs", Parent: d.space,
	}))
	if err != nil {
		t.Fatalf("creating: %v", err)
	}
	document := created.Msg.GetDocument()
	if document.GetId() == "" || document.GetType() != "yjs" || document.GetParent() != d.space {
		t.Fatalf("the document = %v", document)
	}

	got, err := client.Get(ctx, connect.NewRequest(&rookeryv1.DocumentServiceGetRequest{Id: document.GetId()}))
	if err != nil {
		t.Fatalf("getting: %v", err)
	}
	if got.Msg.GetDocument().GetId() != document.GetId() {
		t.Errorf("the document read back = %v", got.Msg.GetDocument())
	}

	// A snapshot of nothing is fine, and a version shows in the history.
	if _, err := client.Snapshot(ctx, connect.NewRequest(&rookeryv1.SnapshotRequest{
		Id: document.GetId(), Sequence: 0, Data: []byte("state"), Version: true, Label: "start",
	})); err != nil {
		t.Fatalf("writing a version: %v", err)
	}
	history, err := client.History(ctx, connect.NewRequest(&rookeryv1.DocumentServiceHistoryRequest{Id: document.GetId()}))
	if err != nil {
		t.Fatalf("reading the history: %v", err)
	}
	if len(history.Msg.GetSnapshots()) != 1 || history.Msg.GetSnapshots()[0].GetLabel() != "start" {
		t.Fatalf("the history = %v", history.Msg.GetSnapshots())
	}

	// A stranger is refused and a missing document is not found.
	stranger := serviceClient(t, d, data.Principal{Kind: data.PrincipalUser, ID: "02USER"})
	_, err = stranger.Get(ctx, connect.NewRequest(&rookeryv1.DocumentServiceGetRequest{Id: document.GetId()}))
	if connect.CodeOf(err) != connect.CodePermissionDenied {
		t.Errorf("a stranger's Get = %v, want PERMISSION_DENIED", err)
	}
	_, err = client.Get(ctx, connect.NewRequest(&rookeryv1.DocumentServiceGetRequest{Id: "missing"}))
	if connect.CodeOf(err) != connect.CodeNotFound {
		t.Errorf("a missing document = %v, want NOT_FOUND", err)
	}

	if _, err := client.Delete(ctx, connect.NewRequest(&rookeryv1.DocumentServiceDeleteRequest{Id: document.GetId()})); err != nil {
		t.Fatalf("deleting: %v", err)
	}
	_, err = client.Get(ctx, connect.NewRequest(&rookeryv1.DocumentServiceGetRequest{Id: document.GetId()}))
	if connect.CodeOf(err) != connect.CodeNotFound {
		t.Errorf("getting a deleted document = %v, want NOT_FOUND", err)
	}
}

// TestServiceSync checks the bidi stream between two clients.
func TestServiceSync(t *testing.T) {
	t.Parallel()

	d := newDocuments(t)
	client := serviceClient(t, d, d.user)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	document := d.created(t, "yjs")
	if _, err := d.Append(t.Context(), d.user, document.ID, []byte("first")); err != nil {
		t.Fatalf("appending: %v", err)
	}
	// The grants the creator got from the granter are on the principal; the
	// second client acts as the same user.

	first := client.Sync(ctx)
	t.Cleanup(func() {
		_ = first.CloseRequest()
		_ = first.CloseResponse()
	})
	if err := first.Send(&rookeryv1.DocumentServiceSyncRequest{
		Message: &rookeryv1.DocumentServiceSyncRequest_Hello{
			Hello: &rookeryv1.SyncHello{DocumentId: document.ID, FromSequence: 0},
		},
	}); err != nil {
		t.Fatalf("sending a hello: %v", err)
	}
	opening, err := receiveResponse(t, first)
	if err != nil {
		t.Fatalf("receiving the state: %v", err)
	}
	if opening.GetUpdate().GetSequence() != 1 || string(opening.GetUpdate().GetData()) != "first" {
		t.Fatalf("the first stream's state = %v", opening)
	}

	second := client.Sync(ctx)
	t.Cleanup(func() {
		_ = second.CloseRequest()
		_ = second.CloseResponse()
	})
	if err := second.Send(&rookeryv1.DocumentServiceSyncRequest{
		Message: &rookeryv1.DocumentServiceSyncRequest_Hello{
			Hello: &rookeryv1.SyncHello{DocumentId: document.ID, FromSequence: 1},
		},
	}); err != nil {
		t.Fatalf("sending a hello: %v", err)
	}

	// An update from the first client reaches the second, with its sequence.
	if err := first.Send(&rookeryv1.DocumentServiceSyncRequest{
		Message: &rookeryv1.DocumentServiceSyncRequest_Update{
			Update: &rookeryv1.SyncUpdate{Data: []byte("second")},
		},
	}); err != nil {
		t.Fatalf("sending an update: %v", err)
	}
	relayed, err := receiveResponse(t, second)
	if err != nil {
		t.Fatalf("receiving the update: %v", err)
	}
	if relayed.GetUpdate().GetSequence() != 2 || string(relayed.GetUpdate().GetData()) != "second" {
		t.Fatalf("the relayed update = %v", relayed)
	}

	// An awareness message is relayed too.
	if err := first.Send(&rookeryv1.DocumentServiceSyncRequest{
		Message: &rookeryv1.DocumentServiceSyncRequest_Awareness{
			Awareness: &rookeryv1.SyncAwareness{Data: []byte("cursor")},
		},
	}); err != nil {
		t.Fatalf("sending awareness: %v", err)
	}
	awareness, err := receiveResponse(t, second)
	if err != nil {
		t.Fatalf("receiving awareness: %v", err)
	}
	if string(awareness.GetAwareness().GetData()) != "cursor" {
		t.Fatalf("the relayed awareness = %v", awareness)
	}
}

// TestServiceSyncRefused checks that a stream for a document the caller cannot
// see ends with a permission error.
func TestServiceSyncRefused(t *testing.T) {
	t.Parallel()

	d := newDocuments(t)
	document := d.created(t, "yjs")
	stranger := serviceClient(t, d, data.Principal{Kind: data.PrincipalUser, ID: "02USER"})
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	stream := stranger.Sync(ctx)
	t.Cleanup(func() {
		_ = stream.CloseRequest()
		_ = stream.CloseResponse()
	})
	if err := stream.Send(&rookeryv1.DocumentServiceSyncRequest{
		Message: &rookeryv1.DocumentServiceSyncRequest_Hello{
			Hello: &rookeryv1.SyncHello{DocumentId: document.ID},
		},
	}); err != nil {
		// A refused stream may end before the send.
		return
	}
	if _, err := stream.Receive(); err == nil {
		t.Fatal("a refused stream stayed open")
	} else if connect.CodeOf(err) != connect.CodePermissionDenied {
		t.Errorf("the stream's error = %v, want PERMISSION_DENIED", err)
	}
}

// receiveResponse reads one stream message.
func receiveResponse(t *testing.T, stream *connect.BidiStreamForClient[rookeryv1.DocumentServiceSyncRequest, rookeryv1.DocumentServiceSyncResponse]) (*rookeryv1.DocumentServiceSyncResponse, error) {
	t.Helper()
	type result struct {
		response *rookeryv1.DocumentServiceSyncResponse
		err      error
	}
	done := make(chan result, 1)
	go func() {
		response, err := stream.Receive()
		done <- result{response, err}
	}()
	select {
	case received := <-done:
		return received.response, received.err
	case <-time.After(5 * time.Second):
		t.Fatalf("no stream message arrived")
		return nil, nil
	}
}
