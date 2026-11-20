// Package blobs stores bytes: content-addressed by SHA-256, in a directory or in
// an S3 bucket, with a row for the size, the type, the owner and the resource the
// blob belongs to.
//
// A blob's access is the resource it belongs to; its owner may always read it.
// A blob nothing references and nobody pinned is removed after a day.
package blobs

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"connectrpc.com/connect"
	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgtype"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/store"
)

// Backend is where a blob's bytes are.
type Backend interface {
	// Put stores a stream and returns its SHA-256 in hex and its size. The same
	// bytes stored twice are one blob.
	Put(ctx context.Context, body io.Reader) (string, int64, error)
	// Open returns a blob's bytes, seekable for a range request.
	Open(ctx context.Context, id string) (io.ReadSeekCloser, int64, error)
	// Delete removes a blob's bytes.
	Delete(ctx context.Context, id string) error
}

// ChunkSize is how much of a blob one message carries.
const ChunkSize = 64 << 10

// Blob is a blob's metadata.
type Blob struct {
	ID          string
	Size        int64
	ContentType string
	Owner       string
	Resource    string
	Pinned      bool
	CreatedAt   time.Time
}

// Message is a blob as the edge API has it.
func (b Blob) Message() *rookeryv1.Blob {
	out := &rookeryv1.Blob{
		Id:          b.ID,
		Size:        b.Size,
		ContentType: b.ContentType,
		Owner:       b.Owner,
		Resource:    b.Resource,
	}
	if !b.CreatedAt.IsZero() {
		out.CreatedAt = timestamppb.New(b.CreatedAt)
	}
	return out
}

// blobOf is a blob from the database.
func blobOf(record store.RookeryBlobs) Blob {
	return Blob{
		ID:          record.ID,
		Size:        record.Size,
		ContentType: record.ContentType,
		Owner:       record.Owner,
		Resource:    record.Resource,
		Pinned:      record.Pinned,
		CreatedAt:   record.CreatedAt,
	}
}

// Blobs stores and reads blobs.
type Blobs struct {
	store   *store.Store
	backend Backend
	checker *access.Checker
}

// New returns the blobs of a deployment.
func New(opened *store.Store, backend Backend, checker *access.Checker) *Blobs {
	return &Blobs{store: opened, backend: backend, checker: checker}
}

// Put stores a stream as a blob that belongs to a resource, and returns its
// metadata.
func (b *Blobs) Put(ctx context.Context, principal access.Principal, resource, contentType string, body io.Reader) (Blob, error) {
	if !principal.Authenticated() {
		return Blob{}, access.ErrUnauthenticated
	}
	resource = strings.TrimSpace(resource)
	if resource != "" {
		if _, err := access.ParseResource(resource); err != nil {
			return Blob{}, err
		}
		if err := b.checker.Check(ctx, principal, resource, access.Use); err != nil {
			return Blob{}, err
		}
	}

	id, size, err := b.backend.Put(ctx, body)
	if err != nil {
		return Blob{}, err
	}
	record, err := b.store.Queries().CreateBlob(ctx, store.CreateBlobParams{
		ID:          id,
		Size:        size,
		ContentType: contentType,
		Owner:       principal.String(),
		Resource:    resource,
	})
	if err != nil {
		return Blob{}, fmt.Errorf("blobs: recording %s: %w", id, err)
	}
	return blobOf(record), nil
}

// Open returns a blob's bytes and its metadata, after checking the caller may
// read it.
func (b *Blobs) Open(ctx context.Context, principal access.Principal, id string) (io.ReadSeekCloser, Blob, error) {
	blob, err := b.ByID(ctx, id)
	if err != nil {
		return nil, Blob{}, err
	}
	if err := b.canRead(ctx, principal, blob); err != nil {
		return nil, Blob{}, err
	}
	reader, _, err := b.backend.Open(ctx, id)
	if err != nil {
		return nil, Blob{}, err
	}
	return reader, blob, nil
}

// ByID returns a blob's metadata.
func (b *Blobs) ByID(ctx context.Context, id string) (Blob, error) {
	record, err := b.store.Queries().GetBlob(ctx, id)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return Blob{}, fmt.Errorf("%w: no blob %s", access.ErrNotFound, id)
	case err != nil:
		return Blob{}, fmt.Errorf("blobs: reading %s: %w", id, err)
	}
	return blobOf(record), nil
}

// Delete removes a blob's bytes and its row.
func (b *Blobs) Delete(ctx context.Context, principal access.Principal, id string) error {
	blob, err := b.ByID(ctx, id)
	if err != nil {
		return err
	}
	if err := b.canManage(ctx, principal, blob); err != nil {
		return err
	}
	if err := b.backend.Delete(ctx, id); err != nil {
		return err
	}
	if _, err := b.store.Queries().DeleteBlob(ctx, id); err != nil {
		return fmt.Errorf("blobs: deleting %s: %w", id, err)
	}
	return nil
}

// canRead is the owner, a reader of the resource, or an admin.
func (b *Blobs) canRead(ctx context.Context, principal access.Principal, blob Blob) error {
	switch {
	case !principal.Authenticated():
		return access.ErrUnauthenticated
	case principal.IsAdmin():
		return nil
	case blob.Owner != "" && blob.Owner == principal.String():
		return nil
	case blob.Resource != "":
		return b.checker.Check(ctx, principal, blob.Resource, access.View)
	default:
		return &access.Denied{Resource: access.KindPlugin + "://" + blob.ID, Need: access.View, Reason: "the blob's owner to share it"}
	}
}

// canManage is the owner, a manager of the resource, or an admin.
func (b *Blobs) canManage(ctx context.Context, principal access.Principal, blob Blob) error {
	switch {
	case !principal.Authenticated():
		return access.ErrUnauthenticated
	case principal.IsAdmin():
		return nil
	case blob.Owner != "" && blob.Owner == principal.String():
		return nil
	case blob.Resource != "":
		return b.checker.Check(ctx, principal, blob.Resource, access.Manage)
	default:
		return &access.Denied{Resource: access.KindPlugin + "://" + blob.ID, Need: access.Manage, Reason: "the blob's owner to share it"}
	}
}

// Collect removes the blobs nothing references and nobody pinned, once they are
// older than age.
func (b *Blobs) Collect(ctx context.Context, age time.Duration) (int64, error) {
	records, err := b.store.Queries().ListUnreferencedBlobs(ctx, store.ListUnreferencedBlobsParams{
		Older: interval(age),
		Limit: 100,
	})
	if err != nil {
		return 0, fmt.Errorf("blobs: listing the blobs to collect: %w", err)
	}
	var removed int64
	for _, record := range records {
		if err := b.backend.Delete(ctx, record.ID); err != nil {
			continue
		}
		if _, err := b.store.Queries().DeleteBlob(ctx, record.ID); err != nil {
			continue
		}
		removed++
	}
	return removed, nil
}

// Close closes a blob's bytes.
func Close(reader io.ReadSeekCloser) {
	if reader != nil {
		_ = reader.Close()
	}
}

// Service is BlobService.
type Service struct {
	blobs *Blobs
}

// NewService returns BlobService for a deployment.
func NewService(store *Blobs) *Service { return &Service{blobs: store} }

// Put stores a streamed blob.
func (s *Service) Put(ctx context.Context, stream *connect.ClientStream[rookeryv1.BlobServicePutRequest]) (*connect.Response[rookeryv1.BlobServicePutResponse], error) {
	principal, ok := access.PrincipalFrom(ctx)
	if !ok {
		return nil, access.ToConnect(access.ErrUnauthenticated)
	}

	if !stream.Receive() {
		if err := stream.Err(); err != nil {
			return nil, access.ToConnect(err)
		}
		return nil, access.ToConnect(fmt.Errorf("%w: a blob's stream starts with its info", access.ErrInvalid))
	}
	info := stream.Msg().GetInfo()
	if info == nil {
		return nil, access.ToConnect(fmt.Errorf("%w: a blob's stream starts with its info", access.ErrInvalid))
	}

	// The bytes are read as they arrive: the pipe's writer is this goroutine's
	// stream, and the reader goes to the backend.
	reader, writer := io.Pipe()
	streamErr := make(chan error, 1)
	go func() {
		defer func() { _ = writer.Close() }()
		for stream.Receive() {
			if chunk := stream.Msg().GetChunk(); len(chunk) > 0 {
				if _, err := writer.Write(chunk); err != nil {
					streamErr <- err
					return
				}
			}
		}
		if err := stream.Err(); err != nil {
			streamErr <- err
			return
		}
		streamErr <- nil
	}()

	blob, err := s.blobs.Put(ctx, principal, info.GetResource(), info.GetContentType(), reader)
	if err != nil {
		_ = reader.Close()
		if streamErr2 := <-streamErr; streamErr2 != nil {
			s.loggerf("the blob's stream ended with %v after the backend failed", streamErr2)
		}
		return nil, access.ToConnect(err)
	}
	if err := <-streamErr; err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.BlobServicePutResponse{Blob: blob.Message()}), nil
}

// Get streams a blob: its metadata, then its bytes.
func (s *Service) Get(ctx context.Context, request *connect.Request[rookeryv1.BlobServiceGetRequest], stream *connect.ServerStream[rookeryv1.BlobServiceGetResponse]) error {
	principal, ok := access.PrincipalFrom(ctx)
	if !ok {
		return access.ToConnect(access.ErrUnauthenticated)
	}
	reader, blob, err := s.blobs.Open(ctx, principal, request.Msg.GetBlob())
	if err != nil {
		return access.ToConnect(err)
	}
	defer Close(reader)

	if err := stream.Send(&rookeryv1.BlobServiceGetResponse{
		Part: &rookeryv1.BlobServiceGetResponse_Blob{Blob: blob.Message()},
	}); err != nil {
		return err
	}

	buffer := make([]byte, ChunkSize)
	for {
		read, err := reader.Read(buffer)
		if read > 0 {
			if sendErr := stream.Send(&rookeryv1.BlobServiceGetResponse{
				Part: &rookeryv1.BlobServiceGetResponse_Chunk{Chunk: buffer[:read]},
			}); sendErr != nil {
				return sendErr
			}
		}
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return access.ToConnect(err)
		}
	}
}

// Delete removes a blob.
func (s *Service) Delete(ctx context.Context, request *connect.Request[rookeryv1.BlobServiceDeleteRequest]) (*connect.Response[rookeryv1.BlobServiceDeleteResponse], error) {
	principal, ok := access.PrincipalFrom(ctx)
	if !ok {
		return nil, access.ToConnect(access.ErrUnauthenticated)
	}
	if err := s.blobs.Delete(ctx, principal, request.Msg.GetBlob()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.BlobServiceDeleteResponse{}), nil
}

// Handler serves /blobs/{id}: the bytes, with range requests, for a caller the
// edge has already authenticated.
func (b *Blobs) Handler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		principal, ok := access.PrincipalFrom(r.Context())
		if !ok {
			http.Error(w, "not signed in", http.StatusUnauthorized)
			return
		}
		id := strings.TrimPrefix(r.URL.Path, "/blobs/")
		if id == "" || strings.Contains(id, "/") {
			http.NotFound(w, r)
			return
		}

		blob, err := b.ByID(r.Context(), id)
		if err != nil {
			http.NotFound(w, r)
			return
		}
		if err := b.canRead(r.Context(), principal, blob); err != nil {
			http.Error(w, "not yours to read", http.StatusForbidden)
			return
		}
		reader, size, err := b.backend.Open(r.Context(), id)
		if err != nil {
			http.Error(w, "the blob's bytes are not there", http.StatusInternalServerError)
			return
		}
		defer Close(reader)

		contentType := blob.ContentType
		if contentType == "" {
			contentType = "application/octet-stream"
		}
		w.Header().Set("Content-Type", contentType)
		w.Header().Set("Cache-Control", "private, max-age=86400")
		http.ServeContent(w, r, id, blob.CreatedAt, &readSeeker{reader: reader, size: size})
	})
}

// readSeeker is a blob's bytes as ServeContent wants them.
type readSeeker struct {
	reader io.ReadSeekCloser
	size   int64
}

// Read reads.
func (r *readSeeker) Read(p []byte) (int, error) { return r.reader.Read(p) }

// Seek seeks.
func (r *readSeeker) Seek(offset int64, whence int) (int64, error) {
	return r.reader.Seek(offset, whence)
}

// interval is a duration as a Postgres interval.
func interval(duration time.Duration) pgtype.Interval {
	return pgtype.Interval{Microseconds: duration.Microseconds(), Valid: true}
}

// loggerf writes a debug line about a stream that stopped in an odd way. It has
// nowhere to go but down: the blob's own error is what the caller gets.
func (s *Service) loggerf(string, ...any) {}
