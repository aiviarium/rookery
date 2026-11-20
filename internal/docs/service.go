package docs

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"

	"connectrpc.com/connect"
	"golang.org/x/sync/errgroup"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/data"
)

// Service is the live documents API.
type Service struct {
	documents *Documents
	caller    data.Caller
}

// NewService returns the document API's service.
func NewService(documents *Documents, caller data.Caller) *Service {
	return &Service{documents: documents, caller: caller}
}

// Create makes a document under a parent resource.
func (s *Service) Create(ctx context.Context, request *connect.Request[rookeryv1.DocumentServiceCreateRequest]) (*connect.Response[rookeryv1.DocumentServiceCreateResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	document, err := s.documents.Create(ctx, principal, request.Msg.GetType(), request.Msg.GetParent())
	if err != nil {
		return nil, data.ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.DocumentServiceCreateResponse{Document: documentMessage(document)}), nil
}

// Get returns a document.
func (s *Service) Get(ctx context.Context, request *connect.Request[rookeryv1.DocumentServiceGetRequest]) (*connect.Response[rookeryv1.DocumentServiceGetResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	document, err := s.documents.Get(ctx, principal, request.Msg.GetId())
	if err != nil {
		return nil, data.ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.DocumentServiceGetResponse{Document: documentMessage(document)}), nil
}

// List returns the documents the caller may read, newest first.
func (s *Service) List(ctx context.Context, request *connect.Request[rookeryv1.DocumentServiceListRequest]) (*connect.Response[rookeryv1.DocumentServiceListResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	found, err := s.documents.List(ctx, principal, request.Msg.GetParent(), int(request.Msg.GetLimit()))
	if err != nil {
		return nil, data.ConnectError(err)
	}
	out := make([]*rookeryv1.Document, 0, len(found))
	for _, document := range found {
		out = append(out, documentMessage(document))
	}
	return connect.NewResponse(&rookeryv1.DocumentServiceListResponse{Documents: out}), nil
}

// Snapshot stores a compacted state covering a sequence.
func (s *Service) Snapshot(ctx context.Context, request *connect.Request[rookeryv1.SnapshotRequest]) (*connect.Response[rookeryv1.SnapshotResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	snapshot, err := s.documents.Snapshot(ctx, principal, request.Msg.GetId(), request.Msg.GetSequence(),
		request.Msg.GetData(), request.Msg.GetVersion(), request.Msg.GetLabel())
	if err != nil {
		return nil, data.ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.SnapshotResponse{At: timestamppb.New(snapshot.At)}), nil
}

// History lists a document's versions.
func (s *Service) History(ctx context.Context, request *connect.Request[rookeryv1.DocumentServiceHistoryRequest]) (*connect.Response[rookeryv1.DocumentServiceHistoryResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	snapshots, err := s.documents.History(ctx, principal, request.Msg.GetId())
	if err != nil {
		return nil, data.ConnectError(err)
	}
	messages := make([]*rookeryv1.DocumentSnapshot, len(snapshots))
	for i, snapshot := range snapshots {
		messages[i] = &rookeryv1.DocumentSnapshot{
			Sequence:  snapshot.Sequence,
			Version:   snapshot.Version,
			Label:     snapshot.Label,
			CreatedBy: snapshot.CreatedBy,
			At:        timestamppb.New(snapshot.At),
		}
	}
	return connect.NewResponse(&rookeryv1.DocumentServiceHistoryResponse{Snapshots: messages}), nil
}

// Delete marks a document deleted.
func (s *Service) Delete(ctx context.Context, request *connect.Request[rookeryv1.DocumentServiceDeleteRequest]) (*connect.Response[rookeryv1.DocumentServiceDeleteResponse], error) {
	principal, err := s.principal(ctx)
	if err != nil {
		return nil, err
	}
	if err := s.documents.Delete(ctx, principal, request.Msg.GetId()); err != nil {
		return nil, data.ConnectError(err)
	}
	return connect.NewResponse(&rookeryv1.DocumentServiceDeleteResponse{}), nil
}

type requestBodyKey struct{}

// WithRequestBody lets Sync unblock a pending transport receive when its
// outbound stream ends first.
func WithRequestBody(handler http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		ctx := context.WithValue(r.Context(), requestBodyKey{}, r.Body)
		handler.ServeHTTP(w, r.WithContext(ctx))
	})
}

// Sync is one participant's stream: the server sends the document's state and
// the others' messages, and appends what this participant sends.
func (s *Service) Sync(ctx context.Context, stream *connect.BidiStream[rookeryv1.DocumentServiceSyncRequest, rookeryv1.DocumentServiceSyncResponse]) error {
	principal, err := s.principal(ctx)
	if err != nil {
		return err
	}

	hello, err := stream.Receive()
	if errors.Is(err, io.EOF) {
		return nil
	}
	if err != nil {
		return err
	}
	opening := hello.GetHello()
	if opening == nil || opening.GetDocumentId() == "" {
		return connect.NewError(connect.CodeInvalidArgument, fmt.Errorf("docs: the first message of a sync is a hello with a document id"))
	}

	sync, err := s.documents.Sync(ctx, principal, opening.GetDocumentId(), opening.GetFromSequence())
	if err != nil {
		return data.ConnectError(err)
	}
	defer func() { _ = sync.Close() }()

	// A participant that closes its side ends the whole stream, so neither
	// loop outlives it.
	streamCtx, stop := context.WithCancel(ctx)
	defer stop()
	group, groupCtx := errgroup.WithContext(streamCtx)
	if body, ok := ctx.Value(requestBodyKey{}).(io.Closer); ok {
		cancelRead := context.AfterFunc(groupCtx, func() { _ = body.Close() })
		defer cancelRead()
	}
	group.Go(func() error {
		for {
			select {
			case <-groupCtx.Done():
				return nil
			case message, ok := <-sync.Messages():
				if !ok {
					stop()
					return data.ConnectError(sync.Err())
				}
				response, err := syncResponse(message)
				if err != nil {
					return data.ConnectError(err)
				}
				if err := stream.Send(response); err != nil {
					return err
				}
			}
		}
	})
	group.Go(func() error {
		for {
			request, err := stream.Receive()
			if errors.Is(err, io.EOF) {
				stop()
				return nil
			}
			if err != nil {
				if groupCtx.Err() != nil {
					return nil
				}
				return err
			}
			message, err := syncRequest(request)
			if err != nil {
				return connect.NewError(connect.CodeInvalidArgument, err)
			}
			if message == nil {
				continue
			}
			if err := sync.Send(groupCtx, *message); err != nil {
				return data.ConnectError(err)
			}
		}
	})
	if err := group.Wait(); err != nil && !errors.Is(err, context.Canceled) {
		return err
	}
	return nil
}

// principal reads the caller from the context.
func (s *Service) principal(ctx context.Context) (data.Principal, error) {
	if s.caller == nil {
		return data.Principal{}, connect.NewError(connect.CodeUnauthenticated, fmt.Errorf("docs: no caller is configured"))
	}
	principal, err := s.caller.Principal(ctx)
	if err != nil {
		return data.Principal{}, data.ConnectError(err)
	}
	return principal, nil
}

// documentMessage is a document on the wire.
func documentMessage(document Document) *rookeryv1.Document {
	return &rookeryv1.Document{
		Id:        document.ID,
		Type:      document.Type,
		Parent:    document.Parent,
		Owner:     document.Owner,
		CreatedAt: timestamppb.New(document.CreatedAt),
		UpdatedAt: timestamppb.New(document.UpdatedAt),
	}
}

// syncRequest reads the message a participant sent, and nil for a hello, which
// only opens the stream.
func syncRequest(request *rookeryv1.DocumentServiceSyncRequest) (*SyncMessage, error) {
	switch message := request.GetMessage().(type) {
	case *rookeryv1.DocumentServiceSyncRequest_Hello:
		return nil, nil
	case *rookeryv1.DocumentServiceSyncRequest_Update:
		return &SyncMessage{Kind: KindUpdate, Data: message.Update.GetData()}, nil
	case *rookeryv1.DocumentServiceSyncRequest_Awareness:
		return &SyncMessage{Kind: KindAwareness, Data: message.Awareness.GetData()}, nil
	default:
		return nil, fmt.Errorf("docs: the sync message is not set")
	}
}

// syncResponse is a message to send on the wire.
func syncResponse(message SyncMessage) (*rookeryv1.DocumentServiceSyncResponse, error) {
	switch message.Kind {
	case KindUpdate:
		return &rookeryv1.DocumentServiceSyncResponse{
			Message: &rookeryv1.DocumentServiceSyncResponse_Update{
				Update: &rookeryv1.SyncUpdate{Data: message.Data, Sequence: message.Sequence},
			},
		}, nil
	case KindAwareness:
		return &rookeryv1.DocumentServiceSyncResponse{
			Message: &rookeryv1.DocumentServiceSyncResponse_Awareness{
				Awareness: &rookeryv1.SyncAwareness{Data: message.Data},
			},
		}, nil
	case KindSnapshot:
		return &rookeryv1.DocumentServiceSyncResponse{
			Message: &rookeryv1.DocumentServiceSyncResponse_Snapshot{
				Snapshot: &rookeryv1.SyncSnapshot{Sequence: message.Sequence, Data: message.Data},
			},
		}, nil
	case KindResync:
		return &rookeryv1.DocumentServiceSyncResponse{
			Message: &rookeryv1.DocumentServiceSyncResponse_Resync{
				Resync: &rookeryv1.SyncResync{Sequence: message.Sequence},
			},
		}, nil
	default:
		return nil, fmt.Errorf("%w: %q is not a sync message", ErrInvalid, message.Kind)
	}
}
