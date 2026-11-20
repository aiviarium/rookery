// Package audit records every call that changes state, and every call that was
// refused: who, which procedure, which resource, and how it ended.
package audit

import (
	"context"
	"fmt"
	"strings"
	"time"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/store"
)

// Outcomes.
const (
	// OK is a call that went through.
	OK = "ok"
	// Denied is a call the access interceptor refused.
	Denied = "denied"
	// Error is a call whose handler failed.
	Error = "error"
)

// detailLimit is how much of an error's text a row keeps.
const detailLimit = 2000

// Entry is one audited call.
type Entry struct {
	At        time.Time
	Principal string
	Procedure string
	Resource  string
	Outcome   string
	Detail    string
}

// Recorder writes and reads the audit trail.
type Recorder struct {
	store *store.Store
}

// NewRecorder returns the audit trail of a database.
func NewRecorder(opened *store.Store) *Recorder { return &Recorder{store: opened} }

// Record writes an entry.
func (r *Recorder) Record(ctx context.Context, entry Entry) error {
	detail := entry.Detail
	if len(detail) > detailLimit {
		detail = detail[:detailLimit]
	}
	_, err := r.store.Queries().InsertAudit(ctx, store.InsertAuditParams{
		Principal: entry.Principal,
		Procedure: entry.Procedure,
		Resource:  entry.Resource,
		Outcome:   entry.Outcome,
		Detail:    detail,
	})
	if err != nil {
		return fmt.Errorf("audit: recording %s: %w", entry.Procedure, err)
	}
	return nil
}

// Filter narrows a listing.
type Filter struct {
	Principal string
	Procedure string
	Resource  string
	After     *time.Time
	Before    *time.Time
	// Cursor is the last id of the page before.
	Cursor int64
	Limit  int
}

// List is a page of entries, newest first.
func (r *Recorder) List(ctx context.Context, filter Filter) ([]store.RookeryAudit, error) {
	limit := filter.Limit
	switch {
	case limit <= 0:
		limit = 100
	case limit > 500:
		limit = 500
	}
	entries, err := r.store.Queries().ListAudit(ctx, store.ListAuditParams{
		Principal: optional(filter.Principal),
		Procedure: optional(filter.Procedure),
		Resource:  optional(filter.Resource),
		After:     filter.After,
		Before:    filter.Before,
		Cursor:    filter.Cursor,
		RowLimit:  int32(limit),
	})
	if err != nil {
		return nil, fmt.Errorf("audit: listing: %w", err)
	}
	return entries, nil
}

// Message is an entry as the edge API has it.
func Message(entry store.RookeryAudit) *rookeryv1.AuditEntry {
	out := &rookeryv1.AuditEntry{
		Id:        entry.ID,
		Principal: entry.Principal,
		Procedure: entry.Procedure,
		Resource:  entry.Resource,
		Outcome:   entry.Outcome,
		Detail:    entry.Detail,
	}
	if !entry.At.IsZero() {
		out.At = timestamppb.New(entry.At)
	}
	return out
}

// Service is AuditService.
type Service struct {
	recorder *Recorder
}

// NewService returns AuditService for a database.
func NewService(recorder *Recorder) *Service { return &Service{recorder: recorder} }

// List returns a page of the audit trail.
func (s *Service) List(ctx context.Context, request *connect.Request[rookeryv1.AuditServiceListRequest]) (*connect.Response[rookeryv1.AuditServiceListResponse], error) {
	filter := Filter{
		Principal: request.Msg.GetPrincipal(),
		Procedure: request.Msg.GetProcedure(),
		Resource:  request.Msg.GetResource(),
		Limit:     int(request.Msg.GetLimit()),
	}
	if at := request.Msg.GetAfter(); at != nil {
		after := at.AsTime()
		filter.After = &after
	}
	if at := request.Msg.GetBefore(); at != nil {
		before := at.AsTime()
		filter.Before = &before
	}
	if cursor := request.Msg.GetCursor(); cursor != "" {
		id, err := parseCursor(cursor)
		if err != nil {
			return nil, access.ToConnect(err)
		}
		filter.Cursor = id
	}

	entries, err := s.recorder.List(ctx, filter)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := make([]*rookeryv1.AuditEntry, 0, len(entries))
	for _, entry := range entries {
		out = append(out, Message(entry))
	}
	next := ""
	if len(out) == max(1, filter.Limit) && len(entries) > 0 {
		next = fmt.Sprint(entries[len(entries)-1].ID)
	}
	return connect.NewResponse(&rookeryv1.AuditServiceListResponse{Entries: out, NextCursor: next}), nil
}

// optional is a filter value, or nil for no filter.
func optional(value string) *string {
	value = strings.TrimSpace(value)
	if value == "" {
		return nil
	}
	return &value
}

// parseCursor reads a cursor this package wrote.
func parseCursor(cursor string) (int64, error) {
	var id int64
	if _, err := fmt.Sscan(cursor, &id); err != nil || id <= 0 {
		return 0, fmt.Errorf("%w: %q is not a cursor", access.ErrInvalid, cursor)
	}
	return id, nil
}
