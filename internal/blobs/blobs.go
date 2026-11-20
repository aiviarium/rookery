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
	"golang.org/x/sync/semaphore"
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
	store     *store.Store
	backend   Backend
	checker   *access.Checker
	mutations *semaphore.Weighted
}

// Deletion is exclusive with uploads, which can otherwise run concurrently.
const exclusiveMutation = 128

// New returns the blobs of a deployment.
func New(opened *store.Store, backend Backend, checker *access.Checker) *Blobs {
	return &Blobs{store: opened, backend: backend, checker: checker, mutations: semaphore.NewWeighted(exclusiveMutation)}
}

// Put stores a stream as a blob that belongs to a resource, and returns its
// metadata.
func (b *Blobs) Put(ctx context.Context, principal access.Principal, resource, contentType string, body io.Reader) (Blob, error) {
	if !principal.Authenticated() {
		return Blob{}, access.ErrUnauthenticated
	}
	resource = strings.TrimSpace(resource)
	if resource != "" {
		kind, err := access.ParseResource(resource)
		if err != nil {
			return Blob{}, err
		}
		if kind == "blob" {
			return Blob{}, fmt.Errorf("%w: a blob cannot be attached to another blob", access.ErrInvalid)
		}
		if err := b.checker.Check(ctx, principal, resource, access.Use); err != nil {
			return Blob{}, err
		}
	}

	if err := b.mutations.Acquire(ctx, 1); err != nil {
		return Blob{}, err
	}
	defer b.mutations.Release(1)

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
	if errors.Is(err, pgx.ErrNoRows) {
		blob, err := b.ByID(ctx, id)
		if err != nil {
			return Blob{}, err
		}
		if err := b.canRead(ctx, principal, blob); err != nil {
			return Blob{}, err
		}
		return blob, nil
	}
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
	if err := b.mutations.Acquire(ctx, exclusiveMutation); err != nil {
		return err
	}
	defer b.mutations.Release(exclusiveMutation)
	blob, err := b.ByID(ctx, id)
	if err != nil {
		return err
	}
	if err := b.canManage(ctx, principal, blob); err != nil {
		return err
	}
	deleted, err := b.remove(ctx, id, 0)
	if err != nil {
		return err
	}
	if !deleted {
		return fmt.Errorf("%w: blob %s still has references", access.ErrConflict, id)
	}
	return nil
}

func (b *Blobs) remove(ctx context.Context, id string, age time.Duration) (bool, error) {
	tx, err := b.store.Pool().Begin(ctx)
	if err != nil {
		return false, fmt.Errorf("blobs: opening deletion transaction: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()
	q := b.store.Queries().WithTx(tx)
	var deleted int64
	if age == 0 {
		deleted, err = q.DeleteBlob(ctx, id)
	} else {
		deleted, err = q.DeleteUnreferencedBlob(ctx, store.DeleteUnreferencedBlobParams{ID: id, Older: interval(age)})
	}
	if err != nil {
		return false, fmt.Errorf("blobs: deleting metadata for %s: %w", id, err)
	}
	if deleted == 0 {
		return false, nil
	}
	if err := b.backend.Delete(ctx, id); err != nil {
		return false, err
	}
	if err := tx.Commit(ctx); err != nil {
		return false, fmt.Errorf("blobs: committing deletion of %s: %w", id, err)
	}
	return true, nil
}

// Pin controls whether collection may remove a blob after its last reference.
func (b *Blobs) Pin(ctx context.Context, principal access.Principal, id string, pinned bool) error {
	blob, err := b.ByID(ctx, id)
	if err != nil {
		return err
	}
	if err := b.canManage(ctx, principal, blob); err != nil {
		return err
	}
	if _, err := b.store.Queries().PinBlob(ctx, store.PinBlobParams{ID: id, Pinned: pinned}); err != nil {
		return fmt.Errorf("blobs: pinning %s: %w", id, err)
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
	if age <= 0 {
		return 0, fmt.Errorf("%w: blob collection age must be positive", access.ErrInvalid)
	}
	records, err := b.store.Queries().ListUnreferencedBlobs(ctx, store.ListUnreferencedBlobsParams{
		Older: interval(age),
		Limit: 100,
	})
	if err != nil {
		return 0, fmt.Errorf("blobs: listing blobs to collect: %w", err)
	}
	var removed int64
	var failures []error
	for _, record := range records {
		if err := b.mutations.Acquire(ctx, exclusiveMutation); err != nil {
			return removed, errors.Join(append(failures, err)...)
		}
		deleted, err := b.remove(ctx, record.ID, age)
		b.mutations.Release(exclusiveMutation)
		if err != nil {
			failures = append(failures, err)
		}
		if deleted {
			removed++
		}
	}
	return removed, errors.Join(failures...)
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

	blob, err := s.blobs.Put(ctx, principal, info.GetResource(), info.GetContentType(), &uploadReader{stream: stream})
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.BlobServicePutResponse{Blob: blob.Message()}), nil
}

type uploadReader struct {
	stream *connect.ClientStream[rookeryv1.BlobServicePutRequest]
	chunk  []byte
}

func (r *uploadReader) Read(buffer []byte) (int, error) {
	for len(r.chunk) == 0 {
		if !r.stream.Receive() {
			if err := r.stream.Err(); err != nil {
				return 0, err
			}
			return 0, io.EOF
		}
		part, ok := r.stream.Msg().GetPart().(*rookeryv1.BlobServicePutRequest_Chunk)
		if !ok {
			return 0, fmt.Errorf("%w: expected a blob chunk", access.ErrInvalid)
		}
		r.chunk = part.Chunk
	}
	count := copy(buffer, r.chunk)
	r.chunk = r.chunk[count:]
	return count, nil
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
		reader, _, err := b.backend.Open(r.Context(), id)
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
		w.Header().Set("Cache-Control", "private, no-store")
		w.Header().Set("Content-Security-Policy", "sandbox")
		w.Header().Set("X-Content-Type-Options", "nosniff")
		http.ServeContent(w, r, id, blob.CreatedAt, reader)
	})
}

// interval is a duration as a Postgres interval.
func interval(duration time.Duration) pgtype.Interval {
	return pgtype.Interval{Microseconds: duration.Microseconds(), Valid: true}
}
