package data

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/filter"
)

// principalKey is where the test middleware puts the caller.
type principalKey struct{}

// headerCaller reads the caller the test middleware put in the context.
type headerCaller struct{}

// Principal is the caller.
func (headerCaller) Principal(ctx context.Context) (Principal, error) {
	principal, ok := ctx.Value(principalKey{}).(Principal)
	if !ok {
		return Principal{}, errors.New("data: no caller")
	}
	return principal, nil
}

// newClient returns a connect client for a datastore, acting as a principal.
// The principal travels as a header, and a middleware puts it in the context,
// which is what the access interceptor will do.
func newClient(t *testing.T, principal Principal, d *datastore) rookeryv1connect.DataServiceClient {
	t.Helper()

	path, handler := rookeryv1connect.NewDataServiceHandler(NewService(d.Data, headerCaller{}))
	mux := http.NewServeMux()
	mux.Handle(path, http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		kind, id, _ := strings.Cut(request.Header.Get("X-Test-Principal"), ":")
		caller := Principal{Kind: PrincipalKind(kind), ID: id}
		handler.ServeHTTP(writer, request.WithContext(context.WithValue(request.Context(), principalKey{}, caller)))
	}))

	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)

	return rookeryv1connect.NewDataServiceClient(server.Client(), server.URL,
		connect.WithGRPC(),
		connect.WithInterceptors(principalInterceptor{principal: principal}),
	)
}

// principalInterceptor sends the test's caller as a header on every call,
// unary and streaming.
type principalInterceptor struct {
	principal Principal
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

// TestServiceRoundTrip drives the service as a client does.
func TestServiceRoundTrip(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	principal := d.writer(t)
	client := newClient(t, principal, d)
	ctx := t.Context()

	inserted, err := client.Insert(ctx, connect.NewRequest(&rookeryv1.InsertRequest{
		Collection: "example/notes",
		RecordCbor: mustEncode(t, map[string]any{
			"title": "Rook notes", "body": "the first", "space_id": space, "count": int64(2),
		}),
	}))
	if err != nil {
		t.Fatalf("inserting: %v", err)
	}
	record := mustDecode(t, inserted.Msg.GetRecordCbor())
	id, _ := record["id"].(string)
	if id == "" || record["rev"] != int64(1) || record["state"] != "open" {
		t.Fatalf("the inserted record = %v", record)
	}

	got, err := client.Get(ctx, connect.NewRequest(&rookeryv1.GetRequest{Collection: "example/notes", Id: id}))
	if err != nil {
		t.Fatalf("getting: %v", err)
	}
	if mustDecode(t, got.Msg.GetRecordCbor())["title"] != "Rook notes" {
		t.Fatalf("the record = %v", mustDecode(t, got.Msg.GetRecordCbor()))
	}

	// A filter from its CBOR structure.
	collection := d.mustCollection(t, "example/notes")
	parsed, err := filter.ParseText("count > 1 and title contains Rook", collection.FilterFields())
	if err != nil {
		t.Fatalf("parsing the filter: %v", err)
	}
	encoded, err := filter.Marshal(parsed)
	if err != nil {
		t.Fatalf("encoding the filter: %v", err)
	}
	found, err := client.Find(ctx, connect.NewRequest(&rookeryv1.FindRequest{
		Collection: "example/notes",
		FilterCbor: encoded,
		Space:      space,
		Limit:      10,
	}))
	if err != nil {
		t.Fatalf("finding: %v", err)
	}
	if len(found.Msg.GetRecordsCbor()) != 1 {
		t.Fatalf("the find returned %d records", len(found.Msg.GetRecordsCbor()))
	}

	counted, err := client.Count(ctx, connect.NewRequest(&rookeryv1.CountRequest{Collection: "example/notes", Space: space}))
	if err != nil {
		t.Fatalf("counting: %v", err)
	}
	if counted.Msg.GetCount() != 1 {
		t.Errorf("count = %d, want 1", counted.Msg.GetCount())
	}

	aggregated, err := client.Aggregate(ctx, connect.NewRequest(&rookeryv1.AggregateRequest{
		Collection: "example/notes",
		Space:      space,
		Operation:  rookeryv1.AggregateOperation_AGGREGATE_OPERATION_SUM,
		Field:      "count",
	}))
	if err != nil {
		t.Fatalf("aggregating: %v", err)
	}
	if len(aggregated.Msg.GetGroupsCbor()) != 1 {
		t.Fatalf("the aggregate returned %d groups", len(aggregated.Msg.GetGroupsCbor()))
	}
	if group := mustDecode(t, aggregated.Msg.GetGroupsCbor()[0]); group["value"] != 2.0 {
		t.Errorf("the aggregate = %v", group)
	}

	updated, err := client.Update(ctx, connect.NewRequest(&rookeryv1.UpdateRequest{
		Collection: "example/notes",
		Id:         id,
		RecordCbor: mustEncode(t, map[string]any{"title": "renamed"}),
		Revision:   1,
	}))
	if err != nil {
		t.Fatalf("updating: %v", err)
	}
	if mustDecode(t, updated.Msg.GetRecordCbor())["rev"] != int64(2) {
		t.Errorf("the updated record = %v", mustDecode(t, updated.Msg.GetRecordCbor()))
	}

	// The old revision is refused as ABORTED.
	_, err = client.Update(ctx, connect.NewRequest(&rookeryv1.UpdateRequest{
		Collection: "example/notes",
		Id:         id,
		RecordCbor: mustEncode(t, map[string]any{"title": "again"}),
		Revision:   1,
	}))
	if connect.CodeOf(err) != connect.CodeAborted {
		t.Fatalf("updating at the old revision = %v, want ABORTED", err)
	}

	if _, err := client.Delete(ctx, connect.NewRequest(&rookeryv1.DeleteRequest{
		Collection: "example/notes", Id: id, Revision: 2,
	})); err != nil {
		t.Fatalf("deleting: %v", err)
	}
	if _, err := client.Get(ctx, connect.NewRequest(&rookeryv1.GetRequest{Collection: "example/notes", Id: id})); connect.CodeOf(err) != connect.CodeNotFound {
		t.Fatalf("getting a deleted record = %v, want NOT_FOUND", err)
	}

	history, err := client.History(ctx, connect.NewRequest(&rookeryv1.HistoryRequest{Collection: "example/notes", Id: id}))
	if err != nil {
		t.Fatalf("history: %v", err)
	}
	if len(history.Msg.GetEntriesCbor()) != 3 {
		t.Errorf("history has %d entries, want 3", len(history.Msg.GetEntriesCbor()))
	}

	restored, err := client.Restore(ctx, connect.NewRequest(&rookeryv1.RestoreRequest{
		Collection: "example/notes", Id: id, Revision: 3,
	}))
	if err != nil {
		t.Fatalf("restoring: %v", err)
	}
	if mustDecode(t, restored.Msg.GetRecordCbor())["rev"] != int64(4) {
		t.Errorf("the restored record = %v", mustDecode(t, restored.Msg.GetRecordCbor()))
	}
}

// TestServiceBatch checks a batch writes all or nothing.
// TestListCollections checks that a caller sees the collections it may browse:
// its own user-scoped ones, a global one it has view on, and a space-scoped one
// only while it holds view on the space.
func TestListCollections(t *testing.T) {
	d := newDatastore(t)
	client := newClient(t, user, d)

	answer, err := client.ListCollections(t.Context(), connect.NewRequest(&rookeryv1.ListCollectionsRequest{Space: space}))
	if err != nil {
		t.Fatal(err)
	}
	if len(answer.Msg.GetCollections()) != 0 {
		t.Fatalf("collections without a grant: %v", answer.Msg.GetCollections())
	}

	d.grant(user, "space://"+space, rookeryv1.Level_LEVEL_VIEW)
	d.grant(user, "plugin://example", rookeryv1.Level_LEVEL_VIEW)
	// A real deployment gives a user manage on their own resource, which is what
	// their user-scoped collections hang from.
	d.grant(user, "user://"+user.ID, rookeryv1.Level_LEVEL_MANAGE)
	answer, err = client.ListCollections(t.Context(), connect.NewRequest(&rookeryv1.ListCollectionsRequest{Space: space}))
	if err != nil {
		t.Fatal(err)
	}
	browsable := map[string]*rookeryv1.Collection{}
	for _, collection := range answer.Msg.GetCollections() {
		browsable[collection.GetPlugin()+"/"+collection.GetName()] = collection
	}
	for _, key := range []string{"example/notes", "example/boards", "example/kinds", "example/drafts"} {
		if browsable[key] == nil {
			t.Fatalf("%s is not listed: %v", key, browsable)
		}
	}
	if !browsable["example/notes"].GetHistory() {
		t.Fatal("the notes collection does not say it keeps history")
	}
	if len(browsable["example/notes"].GetFields()) == 0 {
		t.Fatal("the notes collection lists no fields")
	}
}

func TestServiceBatch(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	client := newClient(t, d.writer(t), d)
	ctx := t.Context()

	written, err := client.Batch(ctx, connect.NewRequest(&rookeryv1.BatchRequest{
		Collection: "example/notes",
		Operations: []*rookeryv1.BatchOperation{
			{Kind: rookeryv1.BatchOperationKind_BATCH_OPERATION_KIND_INSERT, RecordCbor: mustEncode(t, map[string]any{"title": "one", "space_id": space})},
			{Kind: rookeryv1.BatchOperationKind_BATCH_OPERATION_KIND_INSERT, RecordCbor: mustEncode(t, map[string]any{"title": "two", "space_id": space})},
		},
	}))
	if err != nil {
		t.Fatalf("batching: %v", err)
	}
	if len(written.Msg.GetRecordsCbor()) != 2 {
		t.Fatalf("the batch returned %d records", len(written.Msg.GetRecordsCbor()))
	}

	_, err = client.Batch(ctx, connect.NewRequest(&rookeryv1.BatchRequest{
		Collection: "example/notes",
		Operations: []*rookeryv1.BatchOperation{
			{Kind: rookeryv1.BatchOperationKind_BATCH_OPERATION_KIND_INSERT, RecordCbor: mustEncode(t, map[string]any{"title": "three", "space_id": space})},
			{Kind: rookeryv1.BatchOperationKind_BATCH_OPERATION_KIND_INSERT, RecordCbor: mustEncode(t, map[string]any{"space_id": space})},
		},
	}))
	if connect.CodeOf(err) != connect.CodeInvalidArgument {
		t.Fatalf("a batch with a bad operation = %v, want INVALID_ARGUMENT", err)
	}

	counted, err := client.Count(ctx, connect.NewRequest(&rookeryv1.CountRequest{Collection: "example/notes", Space: space}))
	if err != nil {
		t.Fatalf("counting: %v", err)
	}
	if counted.Msg.GetCount() != 2 {
		t.Errorf("count = %d, want 2", counted.Msg.GetCount())
	}
}

// TestServiceErrors checks the codes the service reports.
func TestServiceErrors(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	principal := d.writer(t)
	client := newClient(t, principal, d)
	ctx := t.Context()

	_, err := client.Get(ctx, connect.NewRequest(&rookeryv1.GetRequest{Collection: "example/missing", Id: "x"}))
	if connect.CodeOf(err) != connect.CodeNotFound {
		t.Errorf("an unknown collection = %v, want NOT_FOUND", err)
	}

	_, err = client.Insert(ctx, connect.NewRequest(&rookeryv1.InsertRequest{
		Collection: "example/notes",
		RecordCbor: mustEncode(t, map[string]any{"title": "x", "space_id": space, "missing": true}),
	}))
	if connect.CodeOf(err) != connect.CodeInvalidArgument {
		t.Errorf("an unknown field = %v, want INVALID_ARGUMENT", err)
	}

	// A filter on a field that is not declared.
	badFilter, err := cborx.Marshal(map[string]any{"field": "missing", "value": "x"})
	if err != nil {
		t.Fatalf("encoding the filter: %v", err)
	}
	_, err = client.Find(ctx, connect.NewRequest(&rookeryv1.FindRequest{
		Collection: "example/notes", FilterCbor: badFilter, Space: space,
	}))
	if connect.CodeOf(err) != connect.CodeInvalidArgument {
		t.Errorf("an unknown filter field = %v, want INVALID_ARGUMENT", err)
	}

	// Another user, with no level on the space.
	stranger := newClient(t, other, d)
	_, err = stranger.Find(ctx, connect.NewRequest(&rookeryv1.FindRequest{Collection: "example/notes", Space: space}))
	if connect.CodeOf(err) != connect.CodePermissionDenied {
		t.Errorf("a stranger's find = %v, want PERMISSION_DENIED", err)
	}
}

// TestServiceReferences checks RefsTo and RefsFrom over the service.
func TestServiceReferences(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	principal := d.writer(t)
	client := newClient(t, principal, d)
	ctx := t.Context()

	target, err := client.Insert(ctx, connect.NewRequest(&rookeryv1.InsertRequest{
		Collection: "example/notes",
		RecordCbor: mustEncode(t, map[string]any{"title": "target", "space_id": space}),
	}))
	if err != nil {
		t.Fatalf("inserting the target: %v", err)
	}
	targetID := mustDecode(t, target.Msg.GetRecordCbor())["id"].(string)
	targetURI := "record://example/notes/" + targetID

	board, err := client.Insert(ctx, connect.NewRequest(&rookeryv1.InsertRequest{
		Collection: "example/boards",
		RecordCbor: mustEncode(t, map[string]any{"name": "board", "space_id": space, "note": targetURI}),
	}))
	if err != nil {
		t.Fatalf("inserting the board: %v", err)
	}
	boardURI := "record://example/boards/" + mustDecode(t, board.Msg.GetRecordCbor())["id"].(string)

	to, err := client.RefsTo(ctx, connect.NewRequest(&rookeryv1.RefsToRequest{Target: targetURI}))
	if err != nil {
		t.Fatalf("refs to: %v", err)
	}
	if len(to.Msg.GetReferences()) != 1 || to.Msg.GetReferences()[0].GetSource() != boardURI {
		t.Fatalf("refs to = %v", to.Msg.GetReferences())
	}
	if to.Msg.GetReferences()[0].GetOnDelete() != "cascade" {
		t.Errorf("on delete = %q, want cascade", to.Msg.GetReferences()[0].GetOnDelete())
	}

	from, err := client.RefsFrom(ctx, connect.NewRequest(&rookeryv1.RefsFromRequest{Source: boardURI}))
	if err != nil {
		t.Fatalf("refs from: %v", err)
	}
	if len(from.Msg.GetReferences()) != 1 || from.Msg.GetReferences()[0].GetTarget() != targetURI {
		t.Fatalf("refs from = %v", from.Msg.GetReferences())
	}
}

// TestServiceStreams checks the two streams over a connect client.
func TestServiceStreams(t *testing.T) {
	t.Parallel()

	d := newDatastore(t)
	d.run(t)
	principal := d.writer(t)
	client := newClient(t, principal, d)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	// The record is written before the streams open: a stream sends nothing
	// until it has something to send.
	inserted, err := client.Insert(ctx, connect.NewRequest(&rookeryv1.InsertRequest{
		Collection: "example/notes",
		RecordCbor: mustEncode(t, map[string]any{"title": "streamed", "space_id": space}),
	}))
	if err != nil {
		t.Fatalf("inserting: %v", err)
	}
	id := mustDecode(t, inserted.Msg.GetRecordCbor())["id"].(string)

	live, err := client.Live(ctx, connect.NewRequest(&rookeryv1.LiveRequest{
		Collection: "example/notes", Space: space,
	}))
	if err != nil {
		t.Fatalf("opening the live stream: %v", err)
	}
	changes, err := client.Changes(ctx, connect.NewRequest(&rookeryv1.ChangesRequest{}))
	if err != nil {
		t.Fatalf("opening the changes stream: %v", err)
	}

	if !live.Receive() {
		t.Fatalf("receiving a live event: %v", live.Err())
	}
	event := live.Msg()
	if event.GetKind() != rookeryv1.LiveEventKind_LIVE_EVENT_KIND_INSERT {
		t.Errorf("the live event = %v", event)
	}
	if got := mustDecode(t, event.GetRecordCbor())["id"]; got != id {
		t.Errorf("the live event's record is %v, want %s", got, id)
	}
	if !strings.HasSuffix(event.GetResource(), "/"+id) {
		t.Errorf("the live event's resource is %q", event.GetResource())
	}

	if !changes.Receive() {
		t.Fatalf("receiving a change: %v", changes.Err())
	}
	change := changes.Msg()
	if change.GetKind() != rookeryv1.ChangeKind_CHANGE_KIND_INSERT || change.GetCollection() != "example/notes" {
		t.Errorf("the change = %v", change)
	}
	if change.GetSequence() == 0 || change.GetAt() == nil {
		t.Errorf("the change has no sequence or time: %v", change)
	}
}

// mustEncode writes a value as CBOR or fails.
func mustEncode(t *testing.T, value any) []byte {
	t.Helper()
	encoded, err := cborx.Marshal(value)
	if err != nil {
		t.Fatalf("encoding: %v", err)
	}
	return encoded
}

// mustDecode reads a CBOR map or fails.
func mustDecode(t *testing.T, encoded []byte) map[string]any {
	t.Helper()
	decoded := map[string]any{}
	if err := cborx.Unmarshal(encoded, &decoded); err != nil {
		t.Fatalf("decoding: %v", err)
	}
	return decoded
}

func TestChangesFiltersAccessAndHonorsCursor(t *testing.T) {
	t.Parallel()
	d := newDatastore(t)
	d.run(t)
	writer := d.writer(t)
	d.insertNote(t, writer, map[string]any{"title": "private"})
	d.grant(stranger, "space://02SPACE", rookeryv1.Level_LEVEL_MANAGE)
	for _, title := range []string{"first visible", "second visible"} {
		if _, err := d.Insert(t.Context(), stranger, "example/notes", map[string]any{
			"title": title, "space_id": "02SPACE",
		}); err != nil {
			t.Fatal(err)
		}
	}
	client := newClient(t, stranger, d)
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	stream, err := client.Changes(ctx, connect.NewRequest(&rookeryv1.ChangesRequest{}))
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = stream.Close() }()
	for _, title := range []string{"first visible", "second visible"} {
		if !stream.Receive() {
			t.Fatalf("receiving %q: %v", title, stream.Err())
		}
		if record := mustDecode(t, stream.Msg().GetRecordCbor()); record["title"] != title {
			t.Fatalf("received %v, want %q", record, title)
		}
	}
}
