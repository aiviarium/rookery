package data

import (
	"context"
	"errors"
	"fmt"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/filter"
)

// Caller reads who is calling from the context. The access interceptor puts the
// principal there; this package declares the interface because the principal is
// all it needs.
type Caller interface {
	Principal(ctx context.Context) (Principal, error)
}

// Service is the datastore's edge API.
type Service struct {
	data   *Data
	caller Caller
}

// NewService returns the datastore's service.
func NewService(data *Data, caller Caller) *Service {
	return &Service{data: data, caller: caller}
}

// Get reads one record.
func (s *Service) Get(ctx context.Context, request *connect.Request[rookeryv1.GetRequest]) (*connect.Response[rookeryv1.GetResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	record, err := s.data.Get(ctx, principal, request.Msg.GetCollection(), request.Msg.GetId())
	if err != nil {
		return nil, ConnectError(err)
	}
	encoded, err := encodeRecord(record)
	if err != nil {
		return nil, ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.GetResponse{RecordCbor: encoded}), nil
}

// ListCollections lists the collections the caller may browse: those whose
// scope the caller holds view on.
func (s *Service) ListCollections(ctx context.Context, request *connect.Request[rookeryv1.ListCollectionsRequest]) (*connect.Response[rookeryv1.ListCollectionsResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	space := requestSpace(ctx, request.Msg.GetSpace())

	collections := s.data.Collections("")
	out := make([]*rookeryv1.Collection, 0, len(collections))
	for _, collection := range collections {
		level, err := s.data.levelOn(ctx, principal, collection, space)
		if err != nil {
			return nil, ConnectError(err)
		}
		if !Allows(level, rookeryv1.Level_LEVEL_VIEW) {
			continue
		}
		out = append(out, &rookeryv1.Collection{
			Plugin:  collection.Plugin,
			Name:    collection.Name,
			Scope:   string(collection.Scope),
			Fields:  collection.FieldNames(),
			History: collection.History,
			Level:   level,
		})
	}
	return connect.NewResponse(&rookeryv1.ListCollectionsResponse{Collections: out}), nil
}

// Find reads the records a filter, an order and a window select.
func (s *Service) Find(ctx context.Context, request *connect.Request[rookeryv1.FindRequest]) (*connect.Response[rookeryv1.FindResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	collection, err := s.data.Collection(request.Msg.GetCollection())
	if err != nil {
		return nil, ConnectError(err)
	}
	parsed, err := filter.Parse(request.Msg.GetFilterCbor(), collection.FilterFields())
	if err != nil {
		return nil, ConnectError(err)
	}

	sorts := make([]Sort, len(request.Msg.GetSorts()))
	for i, sort := range request.Msg.GetSorts() {
		sorts[i] = Sort{Field: sort.GetField(), Desc: sort.GetDesc()}
	}
	page, err := s.data.Find(ctx, principal, collection.Key(), Query{
		Filter: parsed,
		Sorts:  sorts,
		Limit:  int(request.Msg.GetLimit()),
		Cursor: request.Msg.GetCursor(),
		Space:  requestSpace(ctx, request.Msg.GetSpace()),
	})
	if err != nil {
		return nil, ConnectError(err)
	}

	records, err := encodeRecords(page.Records)
	if err != nil {
		return nil, ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.FindResponse{RecordsCbor: records, Cursor: page.Cursor}), nil
}

// Count counts the records a filter selects.
func (s *Service) Count(ctx context.Context, request *connect.Request[rookeryv1.CountRequest]) (*connect.Response[rookeryv1.CountResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	collection, err := s.data.Collection(request.Msg.GetCollection())
	if err != nil {
		return nil, ConnectError(err)
	}
	parsed, err := filter.Parse(request.Msg.GetFilterCbor(), collection.FilterFields())
	if err != nil {
		return nil, ConnectError(err)
	}
	count, err := s.data.Count(ctx, principal, collection.Key(), Query{Filter: parsed, Space: requestSpace(ctx, request.Msg.GetSpace())})
	if err != nil {
		return nil, ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.CountResponse{Count: count}), nil
}

// Aggregate computes count, sum, min, max or avg, grouped or not.
func (s *Service) Aggregate(ctx context.Context, request *connect.Request[rookeryv1.AggregateRequest]) (*connect.Response[rookeryv1.AggregateResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	collection, err := s.data.Collection(request.Msg.GetCollection())
	if err != nil {
		return nil, ConnectError(err)
	}
	parsed, err := filter.Parse(request.Msg.GetFilterCbor(), collection.FilterFields())
	if err != nil {
		return nil, ConnectError(err)
	}

	operation, err := aggregateOperation(request.Msg.GetOperation())
	if err != nil {
		return nil, ConnectError(err)
	}
	rows, err := s.data.Aggregate(ctx, principal, collection.Key(), Aggregate{
		Op:      operation,
		Field:   request.Msg.GetField(),
		GroupBy: request.Msg.GetGroupBy(),
		Filter:  parsed,
		Space:   requestSpace(ctx, request.Msg.GetSpace()),
	})
	if err != nil {
		return nil, ConnectError(err)
	}

	groups := make([][]byte, len(rows))
	for i, row := range rows {
		encoded, err := encodeRecord(map[string]any{"group": row.Group, "value": row.Value})
		if err != nil {
			return nil, ConnectError(err)
		}
		groups[i] = encoded
	}
	return connect.NewResponse(&rookeryv1.AggregateResponse{GroupsCbor: groups}), nil
}

// Insert writes a record.
func (s *Service) Insert(ctx context.Context, request *connect.Request[rookeryv1.InsertRequest]) (*connect.Response[rookeryv1.InsertResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	record, err := decodeRecord(request.Msg.GetRecordCbor())
	if err != nil {
		return nil, ConnectError(err)
	}
	written, err := s.data.Insert(ctx, principal, request.Msg.GetCollection(), record)
	if err != nil {
		return nil, ConnectError(err)
	}
	encoded, err := encodeRecord(written)
	if err != nil {
		return nil, ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.InsertResponse{RecordCbor: encoded}), nil
}

// Update changes the fields a record names.
func (s *Service) Update(ctx context.Context, request *connect.Request[rookeryv1.UpdateRequest]) (*connect.Response[rookeryv1.UpdateResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	patch, err := decodeRecord(request.Msg.GetRecordCbor())
	if err != nil {
		return nil, ConnectError(err)
	}
	written, err := s.data.Update(ctx, principal, request.Msg.GetCollection(),
		request.Msg.GetId(), patch, request.Msg.GetRevision())
	if err != nil {
		return nil, ConnectError(err)
	}
	encoded, err := encodeRecord(written)
	if err != nil {
		return nil, ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.UpdateResponse{RecordCbor: encoded}), nil
}

// Upsert writes a record or changes the one its id names.
func (s *Service) Upsert(ctx context.Context, request *connect.Request[rookeryv1.UpsertRequest]) (*connect.Response[rookeryv1.UpsertResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	record, err := decodeRecord(request.Msg.GetRecordCbor())
	if err != nil {
		return nil, ConnectError(err)
	}
	written, err := s.data.Upsert(ctx, principal, request.Msg.GetCollection(), record, request.Msg.GetRevision())
	if err != nil {
		return nil, ConnectError(err)
	}
	encoded, err := encodeRecord(written)
	if err != nil {
		return nil, ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.UpsertResponse{RecordCbor: encoded}), nil
}

// Delete marks a record deleted.
func (s *Service) Delete(ctx context.Context, request *connect.Request[rookeryv1.DeleteRequest]) (*connect.Response[rookeryv1.DeleteResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	err = s.data.Delete(ctx, principal, request.Msg.GetCollection(), request.Msg.GetId(), request.Msg.GetRevision())
	if err != nil {
		return nil, ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.DeleteResponse{}), nil
}

// Restore brings a deleted record back.
func (s *Service) Restore(ctx context.Context, request *connect.Request[rookeryv1.RestoreRequest]) (*connect.Response[rookeryv1.RestoreResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	written, err := s.data.Restore(ctx, principal, request.Msg.GetCollection(), request.Msg.GetId(), request.Msg.GetRevision())
	if err != nil {
		return nil, ConnectError(err)
	}
	encoded, err := encodeRecord(written)
	if err != nil {
		return nil, ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.RestoreResponse{RecordCbor: encoded}), nil
}

// Batch runs several writes on one collection in one transaction.
func (s *Service) Batch(ctx context.Context, request *connect.Request[rookeryv1.BatchRequest]) (*connect.Response[rookeryv1.BatchResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}

	operations := make([]BatchOp, len(request.Msg.GetOperations()))
	for i, operation := range request.Msg.GetOperations() {
		kind, err := batchKind(operation.GetKind())
		if err != nil {
			return nil, ConnectError(err)
		}
		var record map[string]any
		if len(operation.GetRecordCbor()) > 0 {
			record, err = decodeRecord(operation.GetRecordCbor())
			if err != nil {
				return nil, ConnectError(err)
			}
		}
		operations[i] = BatchOp{
			Kind:     kind,
			ID:       operation.GetId(),
			Record:   record,
			Revision: operation.GetRevision(),
		}
	}

	written, err := s.data.Batch(ctx, principal, request.Msg.GetCollection(), operations)
	if err != nil {
		return nil, ConnectError(err)
	}
	records, err := encodeRecords(written)
	if err != nil {
		return nil, ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.BatchResponse{RecordsCbor: records}), nil
}

// History lists a record's revisions.
func (s *Service) History(ctx context.Context, request *connect.Request[rookeryv1.HistoryRequest]) (*connect.Response[rookeryv1.HistoryResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	entries, err := s.data.History(ctx, principal, request.Msg.GetCollection(),
		request.Msg.GetId(), request.Msg.GetFromRevision(), int(request.Msg.GetLimit()))
	if err != nil {
		return nil, ConnectError(err)
	}
	records, err := encodeRecords(entries)
	if err != nil {
		return nil, ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.HistoryResponse{EntriesCbor: records}), nil
}

// RefsTo lists the references that point at a resource.
func (s *Service) RefsTo(ctx context.Context, request *connect.Request[rookeryv1.RefsToRequest]) (*connect.Response[rookeryv1.RefsToResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	refs, err := s.data.RefsTo(ctx, principal, request.Msg.GetTarget())
	if err != nil {
		return nil, ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.RefsToResponse{References: references(refs)}), nil
}

// RefsFrom lists the references a record holds.
func (s *Service) RefsFrom(ctx context.Context, request *connect.Request[rookeryv1.RefsFromRequest]) (*connect.Response[rookeryv1.RefsFromResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	refs, err := s.data.RefsFrom(ctx, principal, request.Msg.GetSource())
	if err != nil {
		return nil, ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.RefsFromResponse{References: references(refs)}), nil
}

// Live follows a query until the caller goes away: the records that match now,
// then each change.
func (s *Service) Live(ctx context.Context, request *connect.Request[rookeryv1.LiveRequest], stream *connect.ServerStream[rookeryv1.LiveResponse]) error {
	principal, err := s.principal(ctx)
	if err != nil {
		return err
	}
	collection, err := s.data.Collection(request.Msg.GetCollection())
	if err != nil {
		return ConnectError(err)
	}
	parsed, err := filter.Parse(request.Msg.GetFilterCbor(), collection.FilterFields())
	if err != nil {
		return ConnectError(err)
	}

	sorts := make([]Sort, len(request.Msg.GetSorts()))
	for i, sort := range request.Msg.GetSorts() {
		sorts[i] = Sort{Field: sort.GetField(), Desc: sort.GetDesc()}
	}

	err = s.data.Live(ctx, principal, LiveOptions{
		Collection: collection.Key(),
		Filter:     parsed,
		Sorts:      sorts,
		Limit:      int(request.Msg.GetLimit()),
		Space:      requestSpace(ctx, request.Msg.GetSpace()),
	}, func(event LiveEvent) error {
		response := &rookeryv1.LiveResponse{
			Kind:     liveEventKind(event.Kind),
			Resource: event.Resource,
		}
		if event.Record != nil {
			encoded, err := encodeRecord(event.Record)
			if err != nil {
				return ConnectError(err)
			}
			response.RecordCbor = encoded
		}
		return stream.Send(response)
	})
	if err != nil {
		return ConnectError(err)
	}
	return nil
}

// Changes streams the change feed from a sequence until the caller goes away.
func (s *Service) Changes(ctx context.Context, request *connect.Request[rookeryv1.ChangesRequest], stream *connect.ServerStream[rookeryv1.ChangesResponse]) error {
	principal, err := s.principal(ctx)
	if err != nil {
		return err
	}

	err = s.data.Changes(ctx, request.Msg.GetFromSequence(), request.Msg.GetCollections(), func(change Change) error {
		collection, err := s.data.Collection(change.Collection)
		if errors.Is(err, ErrNotFound) {
			return nil
		}
		if err != nil {
			return err
		}
		allowed, err := s.data.allowed(ctx, principal, collection.resource(change.scopeID()), collection.Access.Read)
		if err != nil || !allowed {
			return err
		}
		response := &rookeryv1.ChangesResponse{
			Sequence:   change.Seq,
			Collection: change.Collection,
			Resource:   change.Resource,
			Kind:       changeKind(change.Kind),
			Revision:   change.Rev,
			Space:      change.SpaceID,
			Owner:      change.OwnerID,
			Principal:  change.Principal,
			At:         timestamppb.New(change.At),
		}
		if change.Record != nil {
			encoded, err := encodeRecord(change.Record)
			if err != nil {
				return ConnectError(err)
			}
			response.RecordCbor = encoded
		}
		return stream.Send(response)
	})
	if err != nil {
		return ConnectError(err)
	}
	return nil
}

// principal reads the caller from the context.
func (s *Service) principal(ctx context.Context) (Principal, error) {
	if s.caller == nil {
		return Principal{}, connect.NewError(connect.CodeUnauthenticated, errors.New("data: no caller is configured"))
	}
	principal, err := s.caller.Principal(ctx)
	if err != nil {
		return Principal{}, ConnectError(err)
	}
	return principal, nil
}

// encodeRecord writes a record as the CBOR map a request or a response carries.
func encodeRecord(record map[string]any) ([]byte, error) {
	encoded, err := cborx.Marshal(record)
	if err != nil {
		return nil, fmt.Errorf("%w: encoding a record: %w", ErrInvalid, err)
	}
	return encoded, nil
}

// encodeRecords writes records as CBOR maps.
func encodeRecords(records []map[string]any) ([][]byte, error) {
	encoded := make([][]byte, len(records))
	for i, record := range records {
		value, err := encodeRecord(record)
		if err != nil {
			return nil, err
		}
		encoded[i] = value
	}
	return encoded, nil
}

// decodeRecord reads the CBOR map a request carries.
func decodeRecord(encoded []byte) (map[string]any, error) {
	if len(encoded) == 0 {
		return nil, fmt.Errorf("%w: a record is a CBOR map", ErrInvalid)
	}
	record := map[string]any{}
	if err := cborx.Unmarshal(encoded, &record); err != nil {
		return nil, fmt.Errorf("%w: %w", ErrInvalid, err)
	}
	return record, nil
}

// references turns reference rows into the wire's.
func references(refs []Ref) []*rookeryv1.Reference {
	references := make([]*rookeryv1.Reference, len(refs))
	for i, ref := range refs {
		references[i] = &rookeryv1.Reference{
			Source:   ref.Source,
			Field:    ref.Field,
			Target:   ref.Target,
			OnDelete: string(ref.OnDelete),
		}
	}
	return references
}

// liveEventKind maps a live event to the wire's.
func liveEventKind(kind LiveKind) rookeryv1.LiveEventKind {
	switch kind {
	case LiveInsert:
		return rookeryv1.LiveEventKind_LIVE_EVENT_KIND_INSERT
	case LiveUpdate:
		return rookeryv1.LiveEventKind_LIVE_EVENT_KIND_UPDATE
	case LiveRemove:
		return rookeryv1.LiveEventKind_LIVE_EVENT_KIND_REMOVE
	default:
		return rookeryv1.LiveEventKind_LIVE_EVENT_KIND_RESYNC
	}
}

// changeKind maps a change to the wire's.
func changeKind(kind ChangeKind) rookeryv1.ChangeKind {
	switch kind {
	case ChangeInsert:
		return rookeryv1.ChangeKind_CHANGE_KIND_INSERT
	case ChangeUpdate:
		return rookeryv1.ChangeKind_CHANGE_KIND_UPDATE
	case ChangeDelete:
		return rookeryv1.ChangeKind_CHANGE_KIND_DELETE
	case ChangeUndelete:
		return rookeryv1.ChangeKind_CHANGE_KIND_UNDELETE
	default:
		return rookeryv1.ChangeKind_CHANGE_KIND_UNSPECIFIED
	}
}

// aggregateOperation maps the wire's operation to the datastore's.
func aggregateOperation(operation rookeryv1.AggregateOperation) (AggregateOp, error) {
	switch operation {
	case rookeryv1.AggregateOperation_AGGREGATE_OPERATION_COUNT:
		return AggregateCount, nil
	case rookeryv1.AggregateOperation_AGGREGATE_OPERATION_SUM:
		return AggregateSum, nil
	case rookeryv1.AggregateOperation_AGGREGATE_OPERATION_MIN:
		return AggregateMin, nil
	case rookeryv1.AggregateOperation_AGGREGATE_OPERATION_MAX:
		return AggregateMax, nil
	case rookeryv1.AggregateOperation_AGGREGATE_OPERATION_AVG:
		return AggregateAvg, nil
	default:
		return "", fmt.Errorf("%w: the aggregate operation is not set", ErrInvalid)
	}
}

// batchKind maps the wire's operation to the datastore's.
func batchKind(kind rookeryv1.BatchOperationKind) (BatchKind, error) {
	switch kind {
	case rookeryv1.BatchOperationKind_BATCH_OPERATION_KIND_INSERT:
		return BatchInsert, nil
	case rookeryv1.BatchOperationKind_BATCH_OPERATION_KIND_UPDATE:
		return BatchUpdate, nil
	case rookeryv1.BatchOperationKind_BATCH_OPERATION_KIND_UPSERT:
		return BatchUpsert, nil
	case rookeryv1.BatchOperationKind_BATCH_OPERATION_KIND_DELETE:
		return BatchDelete, nil
	case rookeryv1.BatchOperationKind_BATCH_OPERATION_KIND_RESTORE:
		return BatchRestore, nil
	default:
		return "", fmt.Errorf("%w: the batch operation kind is not set", ErrInvalid)
	}
}

// ConnectError classifies filter errors before the shared RPC error mapping.
func ConnectError(err error) error {
	if errors.Is(err, filter.ErrInvalid) || errors.Is(err, filter.ErrUnknownField) ||
		errors.Is(err, filter.ErrUnsupported) || errors.Is(err, filter.ErrUnvalidated) {
		err = fmt.Errorf("%w: %w", access.ErrInvalid, err)
	}
	return access.ToConnect(err)
}

func requestSpace(ctx context.Context, explicit string) string {
	if explicit != "" {
		return explicit
	}
	return access.SpaceFrom(ctx)
}
