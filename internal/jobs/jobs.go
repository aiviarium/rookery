// Package jobs is what Rookery has to do: a job's record, its arguments, its log,
// its attempts, who may enqueue its kind, and the runners that work it.
//
// A core kind runs here, in the server, one attempt and no automatic retry, and
// at most one per host. A plugin kind is claimed by the plugin's own service
// through Worker, with a heartbeat, retries with backoff and a claim token that
// says the claim is still the same one.
package jobs

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"math"
	"strings"
	"sync"
	"time"

	"github.com/jackc/pgx/v5"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/id"
	"github.com/aiviarium/rookery/internal/store"
)

// State is where a job is.
type State string

// The states a job is in.
const (
	Queued      State = "queued"
	Running     State = "running"
	Succeeded   State = "succeeded"
	Failed      State = "failed"
	Cancelled   State = "cancelled"
	Interrupted State = "interrupted"
)

// Done reports whether a job has ended.
func (s State) Done() bool {
	switch s {
	case Succeeded, Failed, Cancelled, Interrupted:
		return true
	default:
		return false
	}
}

// Kind is a kind of job: who may enqueue it, how long it may take, how often it
// is retried, and whether only one of it runs per host.
type Kind struct {
	// Name is such as host.apply or example/reindex.
	Name string
	// Core is set for the kinds the server runs in process.
	Core bool
	// Level is what a caller needs on the job's resource to enqueue it.
	Level access.Level
	// Timeout bounds one attempt. Zero means no bound.
	Timeout time.Duration
	// Retries is how many times a failed attempt is tried again. Core kinds
	// have none.
	Retries int
	// Backoff is how long a retry waits, rising with each attempt.
	Backoff time.Duration
	// HostLock is set for a kind that is about one host: at most one of them
	// runs per host at a time.
	HostLock bool
}

// CoreKinds are the kinds the server runs in process. Their handlers arrive with
// the items that need them: hosts, apply and provisioners.
func CoreKinds() []Kind {
	return []Kind{
		{Name: "host.contact", Core: true, Level: access.Manage, HostLock: true, Timeout: 10 * time.Minute},
		{Name: "host.apply", Core: true, Level: access.Manage, HostLock: true, Timeout: time.Hour},
		{Name: "host.action", Core: true, Level: access.Manage, HostLock: true, Timeout: time.Hour},
		{Name: "host.create", Core: true, Level: access.Manage, HostLock: true, Timeout: time.Hour},
		{Name: "host.remove", Core: true, Level: access.Manage, HostLock: true, Timeout: 30 * time.Minute},
		{Name: "provisioner.action", Core: true, Level: access.Manage, Timeout: 30 * time.Minute},
	}
}

// Handler runs a job in process.
type Handler interface {
	// Run works the job and returns its result, which the caller sees as CBOR.
	// The log is where its output goes; every line is masked on the way in.
	Run(ctx context.Context, job Job, log *Log) ([]byte, error)
}

// HandlerFunc is a function that is a Handler.
type HandlerFunc func(ctx context.Context, job Job, log *Log) ([]byte, error)

// Run works the job.
func (f HandlerFunc) Run(ctx context.Context, job Job, log *Log) ([]byte, error) {
	return f(ctx, job, log)
}

// Registry is the kinds this server knows, and the handlers of the ones that run
// here. It is filled at start-up, and a plugin's kinds join it when the plugin is
// installed, so it is safe for concurrent use.
type Registry struct {
	mu       sync.RWMutex
	kinds    map[string]Kind
	handlers map[string]Handler
}

// NewRegistry returns an empty registry.
func NewRegistry() *Registry {
	return &Registry{kinds: make(map[string]Kind), handlers: make(map[string]Handler)}
}

// Register adds a kind and its in-process handler.
func (r *Registry) Register(kind Kind, handler Handler) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if kind.Name == "" {
		return errors.New("jobs: a kind needs a name")
	}
	if _, seen := r.kinds[kind.Name]; seen {
		return fmt.Errorf("jobs: the kind %s is there already", kind.Name)
	}
	if kind.Level == accessLevelUnspecified {
		kind.Level = access.Use
	}
	if kind.Core && kind.Retries != 0 {
		return fmt.Errorf("jobs: the core kind %s cannot have retries", kind.Name)
	}
	r.kinds[kind.Name] = kind
	r.handlers[kind.Name] = handler
	return nil
}

// RegisterKinds adds kinds without handlers, for the ones another item gives a
// handler.
func (r *Registry) RegisterKinds(kinds ...Kind) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	for _, kind := range kinds {
		if _, seen := r.kinds[kind.Name]; seen {
			return fmt.Errorf("jobs: the kind %s is there already", kind.Name)
		}
		if kind.Level == accessLevelUnspecified {
			kind.Level = access.Use
		}
		r.kinds[kind.Name] = kind
	}
	return nil
}

// Handle gives an existing kind its in-process handler, for the item that
// arrives after the kind's declaration.
func (r *Registry) Handle(name string, handler Handler) error {
	if _, ok := r.kinds[name]; !ok {
		return fmt.Errorf("jobs: there is no kind %s to handle", name)
	}
	if handler == nil {
		return errors.New("jobs: a handler cannot be nil")
	}
	r.handlers[name] = handler
	return nil
}

// Kind returns a kind.
func (r *Registry) Kind(name string) (Kind, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	kind, ok := r.kinds[name]
	return kind, ok
}

// Kinds returns every kind, for a listing.
func (r *Registry) Kinds() []Kind {
	r.mu.RLock()
	defer r.mu.RUnlock()
	out := make([]Kind, 0, len(r.kinds))
	for _, kind := range r.kinds {
		out = append(out, kind)
	}
	return out
}

// handler is the handler of a kind, if it has one.
func (r *Registry) handler(name string) (Handler, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	handler, ok := r.handlers[name]
	return handler, ok && handler != nil
}

// runnable is the core kinds this server can run now.
func (r *Registry) runnable() []string {
	r.mu.RLock()
	defer r.mu.RUnlock()
	out := make([]string, 0, len(r.handlers))
	for name, handler := range r.handlers {
		if kind, ok := r.kinds[name]; ok && kind.Core && handler != nil {
			out = append(out, name)
		}
	}
	return out
}

// accessLevelUnspecified is a kind with no level declared.
const accessLevelUnspecified = rookeryv1.Level_LEVEL_UNSPECIFIED

// Job is a job's record.
type Job struct {
	ID          string
	Kind        string
	Args        []byte
	Resource    string
	CreatedBy   string
	Parent      string
	State       State
	Attempts    int
	MaxAttempts int
	Host        string
	Result      []byte
	Err         string
	CancelAsked bool
	CreatedAt   time.Time
	StartedAt   time.Time
	FinishedAt  time.Time
}

// JobOf is a job's record from the database.
func JobOf(record store.RookeryJobs) Job {
	job := Job{
		ID:          record.ID,
		Kind:        record.Kind,
		Args:        record.Args,
		Resource:    record.Resource,
		CreatedBy:   record.CreatedBy,
		Parent:      record.Parent,
		State:       State(record.State),
		Attempts:    int(record.Attempts),
		MaxAttempts: int(record.MaxAttempts),
		Host:        record.Host,
		Result:      record.Result,
		Err:         record.Error,
		CancelAsked: record.CancelRequested,
	}
	if !record.CreatedAt.IsZero() {
		job.CreatedAt = record.CreatedAt
	}
	if record.StartedAt != nil {
		job.StartedAt = *record.StartedAt
	}
	if record.FinishedAt != nil {
		job.FinishedAt = *record.FinishedAt
	}
	return job
}

// Message is a job as the edge API has it.
func Message(job Job) *rookeryv1.Job {
	out := &rookeryv1.Job{
		Id:          job.ID,
		Kind:        job.Kind,
		ArgsCbor:    job.Args,
		Resource:    job.Resource,
		CreatedBy:   job.CreatedBy,
		Parent:      job.Parent,
		State:       string(job.State),
		Attempts:    int32(min(job.Attempts, math.MaxInt32)),    //nolint:gosec // clamped to int32
		MaxAttempts: int32(min(job.MaxAttempts, math.MaxInt32)), //nolint:gosec // clamped to int32
		ResultCbor:  job.Result,
		Error:       job.Err,
	}
	if !job.CreatedAt.IsZero() {
		out.CreatedAt = timestamppb.New(job.CreatedAt)
	}
	if !job.StartedAt.IsZero() {
		out.StartedAt = timestamppb.New(job.StartedAt)
	}
	if !job.FinishedAt.IsZero() {
		out.FinishedAt = timestamppb.New(job.FinishedAt)
	}
	return out
}

// Publisher is what the runner tells the events bus about a job changing.
type Publisher interface {
	Publish(name, resource string, payload []byte)
}

// publisher is a Publisher that tells nobody, for a runner without an event bus.
type quietPublisher struct{}

// Publish does nothing.
func (quietPublisher) Publish(string, string, []byte) {}

// Service is JobService: enqueueing jobs, reading them and following them.
type Service struct {
	store    *store.Store
	checker  *access.Checker
	registry *Registry
	runner   *Runner
	logger   *slog.Logger
}

// NewService returns JobService for a database.
func NewService(opened *store.Store, checker *access.Checker, registry *Registry, runner *Runner, logger *slog.Logger) *Service {
	if logger == nil {
		logger = slog.New(slog.DiscardHandler)
	}
	return &Service{store: opened, checker: checker, registry: registry, runner: runner, logger: logger}
}

// EnqueueRequest is what a caller asks for.
type EnqueueRequest struct {
	Kind     string
	Args     []byte
	Resource string
	Parent   string
}

// Enqueue makes a job, and grants its caller manage on it, so that the job is
// theirs to follow and cancel.
func (s *Service) Enqueue(ctx context.Context, request EnqueueRequest) (Job, error) {
	principal, ok := access.PrincipalFrom(ctx)
	if !ok {
		return Job{}, access.ErrUnauthenticated
	}
	return s.enqueue(ctx, request, principal, nil)
}

// EnqueueLinked persists a job and its owning operation's update together.
func (s *Service) EnqueueLinked(ctx context.Context, request EnqueueRequest, principal access.Principal,
	link func(context.Context, *store.Queries, Job) error) (Job, error) {
	return s.enqueue(ctx, request, principal, link)
}

// enqueue is Enqueue for a caller the caller has already resolved.
func (s *Service) enqueue(ctx context.Context, request EnqueueRequest, principal access.Principal, link func(context.Context, *store.Queries, Job) error) (Job, error) {
	kind, known := s.registry.Kind(request.Kind)
	if !known {
		return Job{}, fmt.Errorf("%w: there is no job kind %q", access.ErrInvalid, request.Kind)
	}

	resource := strings.TrimSpace(request.Resource)
	if resource != "" {
		if _, err := access.ParseResource(resource); err != nil {
			return Job{}, err
		}
	}
	if err := s.checker.Check(ctx, principal, resource, kind.Level); err != nil {
		return Job{}, err
	}

	jobID := id.New()
	host := ""
	if kind.HostLock {
		if kind, err := access.ParseResource(resource); err != nil {
			return Job{}, fmt.Errorf("%w: the kind %s is about one host, so it needs a host resource", access.ErrInvalid, request.Kind)
		} else if kind == access.KindHost {
			host = strings.TrimPrefix(resource, access.KindHost+"://")
		} else {
			return Job{}, fmt.Errorf("%w: the kind %s is about one host, not %s", access.ErrInvalid, request.Kind, resource)
		}
	}

	maxAttempts := 1
	if !kind.Core && kind.Retries > 0 {
		maxAttempts = kind.Retries + 1
	}

	tx, err := s.store.Pool().Begin(ctx)
	if err != nil {
		return Job{}, fmt.Errorf("jobs: beginning the enqueue: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	queries := s.store.Queries().WithTx(tx)
	record, err := queries.CreateJob(ctx, store.CreateJobParams{
		ID:          jobID,
		Kind:        request.Kind,
		Args:        nonNil(request.Args),
		Resource:    resource,
		CreatedBy:   principal.String(),
		Parent:      request.Parent,
		MaxAttempts: int32(maxAttempts),
		Host:        host,
	})
	if err != nil {
		return Job{}, fmt.Errorf("jobs: recording the job: %w", err)
	}

	// The job is a resource of its own, with the job's resource as its parent,
	// so a grant on that resource reaches the job. The parent has to be a
	// resource row of its own; a caller may enqueue on any resource it can use.
	if resource != "" {
		if err := queries.UpsertResource(ctx, store.UpsertResourceParams{Uri: resource}); err != nil {
			return Job{}, fmt.Errorf("jobs: recording the resource %s: %w", resource, err)
		}
	}
	if err := queries.UpsertResource(ctx, store.UpsertResourceParams{Uri: access.JobURI(jobID), Parent: optional(resource)}); err != nil {
		return Job{}, fmt.Errorf("jobs: recording the job's resource: %w", err)
	}
	if principal.Kind == access.KindUser {
		if _, err := queries.UpsertGrant(ctx, store.UpsertGrantParams{
			Resource:  access.JobURI(jobID),
			Principal: access.UserPrincipal(principal.ID),
			Level:     int16(access.Manage),
		}); err != nil {
			return Job{}, fmt.Errorf("jobs: granting the caller manage on the job: %w", err)
		}
	}

	job := JobOf(record)
	if link != nil {
		if err := link(ctx, queries, job); err != nil {
			return Job{}, err
		}
	}
	if err := tx.Commit(ctx); err != nil {
		return Job{}, fmt.Errorf("jobs: committing the enqueue: %w", err)
	}
	s.publish(job)
	return job, nil
}

// EnqueueSystem makes a job with no caller behind it: a schedule's run.
func (s *Service) EnqueueSystem(ctx context.Context, request EnqueueRequest) (Job, error) {
	return s.enqueue(ctx, request, access.LocalAdmin(), nil)
}

// Get returns a job.
func (s *Service) Get(ctx context.Context, jobID string) (Job, error) {
	record, err := s.store.Queries().GetJob(ctx, jobID)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return Job{}, fmt.Errorf("%w: no job %s", access.ErrNotFound, jobID)
	case err != nil:
		return Job{}, fmt.Errorf("jobs: reading the job %s: %w", jobID, err)
	}
	return JobOf(record), nil
}

// Filter narrows a listing.
type Filter struct {
	Kind     string
	State    string
	Resource string
	Cursor   string
	Limit    int
}

// List is a page of jobs, newest first.
func (s *Service) List(ctx context.Context, filter Filter) ([]Job, error) {
	cursor := filter.Cursor
	if cursor == "" {
		// Ids are ULIDs in Crockford's base32, so "~" sorts after all of them.
		cursor = "~"
	}
	params := store.ListJobsParams{
		Kind:     optional(filter.Kind),
		State:    optional(filter.State),
		Resource: optional(filter.Resource),
		Cursor:   cursor,
		RowLimit: clampLimit(filter.Limit),
	}
	principal, authenticated := access.PrincipalFrom(ctx)
	out := make([]Job, 0, params.RowLimit)
	for len(out) < int(params.RowLimit) {
		records, err := s.store.Queries().ListJobs(ctx, params)
		if err != nil {
			return nil, fmt.Errorf("jobs: listing jobs: %w", err)
		}
		for _, record := range records {
			if authenticated {
				if err := s.checker.Check(ctx, principal, access.JobURI(record.ID), access.View); err != nil {
					if errors.Is(err, access.ErrDenied) {
						continue
					}
					return nil, err
				}
			}
			out = append(out, JobOf(record))
			if len(out) == int(params.RowLimit) {
				break
			}
		}
		if len(records) < int(params.RowLimit) {
			break
		}
		params.Cursor = records[len(records)-1].ID
	}
	return out, nil
}

// Follow calls send with every line after from, and then with the job's state
// once the job has ended. It returns when the job has ended or ctx does.
func (s *Service) Follow(ctx context.Context, jobID string, from int64, send func(line store.RookeryJobLines, done bool, job Job) error) error {
	ticker := time.NewTicker(followInterval)
	defer ticker.Stop()

	after := from
	for {
		job, err := s.Get(ctx, jobID)
		if err != nil {
			return err
		}
		const batch = 200
		lines, err := s.store.Queries().LinesAfter(ctx, store.LinesAfterParams{JobID: jobID, ID: after, Limit: batch})
		if err != nil {
			return fmt.Errorf("jobs: reading the log of %s: %w", jobID, err)
		}
		for _, line := range lines {
			after = line.ID
			if err := send(line, false, Job{}); err != nil {
				return err
			}
		}
		if len(lines) == batch {
			continue
		}
		if job.State.Done() {
			return send(store.RookeryJobLines{}, true, job)
		}
		select {
		case <-ctx.Done():
			return nil
		case <-ticker.C:
		}
	}
}

// Cancel asks a job to stop. A queued job is cancelled at once; a running one is
// told, and ends cancelled when it notices.
func (s *Service) Cancel(ctx context.Context, jobID string) (Job, error) {
	job, err := s.Get(ctx, jobID)
	if err != nil {
		return Job{}, err
	}
	switch job.State {
	case Queued:
		cancelled, err := s.store.Queries().CancelQueuedJob(ctx, jobID)
		if err != nil {
			return Job{}, fmt.Errorf("jobs: cancelling %s: %w", jobID, err)
		}
		if cancelled == 0 {
			return s.Cancel(ctx, jobID)
		}
	case Running:
		asked, err := s.store.Queries().RequestCancel(ctx, jobID)
		if err != nil {
			return Job{}, fmt.Errorf("jobs: asking %s to stop: %w", jobID, err)
		}
		if asked > 0 && s.runner != nil {
			s.runner.cancel(jobID)
		}
	case Succeeded, Failed, Cancelled, Interrupted:
		return Job{}, fmt.Errorf("%w: the job %s has ended", access.ErrConflict, jobID)
	}

	job, err = s.Get(ctx, jobID)
	if err != nil {
		return Job{}, err
	}
	s.publish(job)
	return job, nil
}

// Retry puts a job back in the queue, as it was before its first attempt.
func (s *Service) Retry(ctx context.Context, jobID string) (Job, error) {
	record, err := s.store.Queries().RequeueJob(ctx, jobID)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		if _, err := s.Get(ctx, jobID); err != nil {
			return Job{}, err
		}
		return Job{}, fmt.Errorf("%w: job %s is active", access.ErrConflict, jobID)
	case err != nil:
		return Job{}, fmt.Errorf("jobs: retrying %s: %w", jobID, err)
	}
	job := JobOf(record)
	s.publish(job)
	return job, nil
}

// publish says a job changed, for the servers watching.
func (s *Service) publish(job Job) {
	if s.runner == nil {
		return
	}
	s.runner.publisher.Publish(eventJobChanged, job.Resource, jobEvent(job))
}

// eventJobChanged is the core event for a job changing.
const eventJobChanged = "rookery/jobs.changed"

// jobEvent is a job's change as an event's payload.
func jobEvent(job Job) []byte {
	payload, err := cborx.Marshal(map[string]any{"id": job.ID, "kind": job.Kind, "state": string(job.State)})
	if err != nil {
		return nil
	}
	return payload
}

// followInterval is how often Follow looks again.
const followInterval = 200 * time.Millisecond

// clampLimit bounds a page size.
func clampLimit(limit int) int32 {
	switch {
	case limit <= 0:
		return 100
	case limit > 500:
		return 500
	default:
		return int32(limit)
	}
}

// optional is a filter value, or nil for no filter.
func optional(value string) *string {
	value = strings.TrimSpace(value)
	if value == "" {
		return nil
	}
	return &value
}

// nonNil is bytes the database can store: an empty array rather than nil.
func nonNil(value []byte) []byte {
	if value == nil {
		return []byte{}
	}
	return value
}
