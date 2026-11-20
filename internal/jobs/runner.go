package jobs

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"sync"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgtype"
	"golang.org/x/sync/errgroup"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/id"
	"github.com/aiviarium/rookery/internal/store"
)

// Defaults of the runner.
const (
	// DefaultPoll is how often the runner looks for a job when there is none.
	DefaultPoll = 250 * time.Millisecond
	// DefaultWorkers is how many core jobs run at once. The per-host lock
	// keeps two of them off one host.
	DefaultWorkers = 4
	// claimTimeout is how long a plugin may go without a heartbeat before its
	// claim is taken back.
	claimTimeout = 60 * time.Second
	// reapInterval is how often expired claims are looked for.
	reapInterval = 15 * time.Second
)

// Runner runs the core kinds: it claims queued jobs of the kinds this server has
// handlers for, runs them in process, and records how they ended.
type Runner struct {
	store     *store.Store
	registry  *Registry
	logger    *slog.Logger
	publisher Publisher
	onFailure func(context.Context, Job)

	poll      time.Duration
	workers   int
	cancelMu  sync.Mutex
	cancels   map[string]context.CancelFunc
	hostsMu   sync.Mutex
	busyHosts map[string]bool
}

// NewRunner returns a runner for a registry.
func NewRunner(opened *store.Store, registry *Registry, publisher Publisher, logger *slog.Logger) *Runner {
	if publisher == nil {
		publisher = quietPublisher{}
	}
	if logger == nil {
		logger = slog.New(slog.DiscardHandler)
	}
	return &Runner{
		store:     opened,
		registry:  registry,
		logger:    logger,
		publisher: publisher,
		poll:      DefaultPoll,
		workers:   DefaultWorkers,
		cancels:   make(map[string]context.CancelFunc),
		busyHosts: make(map[string]bool),
	}
}

// OnFailure sets the handler for terminal core job failures before Run starts.
func (r *Runner) OnFailure(notify func(context.Context, Job)) { r.onFailure = notify }

// Run works jobs until ctx ends. It claims one job at a time, in this goroutine,
// and runs each in a goroutine of its own, bounded by the runner's workers.
func (r *Runner) Run(ctx context.Context) error {
	group, ctx := errgroup.WithContext(ctx)
	slots := make(chan struct{}, r.workers)
	reap := time.NewTicker(reapInterval)
	defer reap.Stop()

	for {
		if err := ctx.Err(); err != nil {
			break
		}

		select {
		case <-reap.C:
			r.reap(ctx)
		default:
		}

		job, err := r.claim(ctx)
		switch {
		case errors.Is(err, pgx.ErrNoRows):
			select {
			case <-ctx.Done():
			case <-time.After(r.poll):
			}
			continue
		case err != nil:
			r.logger.Error("claiming a job", slog.String("error", err.Error()))
			select {
			case <-ctx.Done():
			case <-time.After(r.poll):
			}
			continue
		}

		select {
		case slots <- struct{}{}:
		case <-ctx.Done():
			r.releaseHost(job.Host)
			recordCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), writeTimeout)
			r.end(recordCtx, job, Interrupted, "the server stopped")
			cancel()
			continue
		}
		group.Go(func() error {
			defer func() { <-slots }()
			r.work(ctx, job)
			return nil
		})
	}

	if err := group.Wait(); err != nil {
		return err
	}
	return ctx.Err()
}

// claim takes the next queued job of a kind this server runs, skipping the hosts
// one of them is already working on. The job's host is booked here, before the
// goroutine starts, so no later claim picks it too.
func (r *Runner) claim(ctx context.Context) (Job, error) {
	kinds := r.registry.runnable()
	if len(kinds) == 0 {
		return Job{}, pgx.ErrNoRows
	}

	record, err := r.store.Queries().ClaimCoreJob(ctx, store.ClaimCoreJobParams{
		Kinds:     kinds,
		BusyHosts: r.busy(),
	})
	if err != nil {
		return Job{}, fmt.Errorf("jobs: claiming a job: %w", err)
	}
	job := JobOf(record)
	if job.Host != "" {
		r.hostsMu.Lock()
		r.busyHosts[job.Host] = true
		r.hostsMu.Unlock()
	}
	return job, nil
}

// busy is the hosts a job is running on.
func (r *Runner) busy() []string {
	r.hostsMu.Lock()
	defer r.hostsMu.Unlock()
	out := make([]string, 0, len(r.busyHosts))
	for host := range r.busyHosts {
		out = append(out, host)
	}
	return out
}

// releaseHost unbooks a host when its job has ended.
func (r *Runner) releaseHost(host string) {
	if host == "" {
		return
	}
	r.hostsMu.Lock()
	delete(r.busyHosts, host)
	r.hostsMu.Unlock()
}

// work runs one job and records how it ended.
func (r *Runner) work(ctx context.Context, job Job) {
	defer r.releaseHost(job.Host)

	kind, known := r.registry.Kind(job.Kind)
	handler, hasHandler := r.registry.handler(job.Kind)
	if !known || !hasHandler {
		// A kind an item later gives a handler: it waits rather than failing.
		r.logger.Warn("no handler for a job this server claimed", slog.String("kind", job.Kind))
		return
	}

	jobCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	if kind.Timeout > 0 {
		var cancelTimeout context.CancelFunc
		jobCtx, cancelTimeout = context.WithTimeout(jobCtx, kind.Timeout)
		defer cancelTimeout()
	}

	r.cancelMu.Lock()
	r.cancels[job.ID] = cancel
	r.cancelMu.Unlock()
	defer func() {
		r.cancelMu.Lock()
		delete(r.cancels, job.ID)
		r.cancelMu.Unlock()
	}()

	log := NewLog(jobCtx, r.store, job.ID, NewMasker())
	asked, err := r.store.Queries().CancelRequested(jobCtx, job.ID)
	var result []byte
	var runErr error
	if err == nil && !asked {
		result, runErr = handler.Run(jobCtx, job, log)
	} else {
		runErr = err
	}
	recordCtx, recordCancel := context.WithTimeout(context.WithoutCancel(ctx), writeTimeout)
	defer recordCancel()
	asked, askErr := r.store.Queries().CancelRequested(recordCtx, job.ID)
	if askErr == nil && asked {
		r.end(recordCtx, job, Cancelled, "cancelled")
		return
	}
	if ctx.Err() != nil {
		r.end(recordCtx, job, Interrupted, "the server stopped")
		return
	}
	switch {
	case runErr != nil:
		r.fail(recordCtx, job, kind, runErr, log)
	default:
		if _, err := r.store.Queries().CompleteJob(recordCtx, store.CompleteJobParams{
			Result: nonNil(result),
			ID:     job.ID,
		}); err != nil {
			r.logger.Error("recording a finished job", slog.String("job", job.ID), slog.String("error", err.Error()))
			return
		}
		r.publish(job, Succeeded)
	}
}

// fail records a failed attempt, and queues another when the kind retries.
func (r *Runner) fail(ctx context.Context, job Job, kind Kind, cause error, log *Log) {
	_ = log.Write(fmt.Sprintf("failed: %v", cause)) //nolint:contextcheck // the log carries the job's context

	if job.Attempts < job.MaxAttempts {
		backoff := retryBackoff(kind, job.Attempts)
		failed, err := r.store.Queries().RetryJob(ctx, store.RetryJobParams{
			Error:   log.mask(cause.Error()),
			ID:      job.ID,
			Backoff: interval(backoff),
		})
		if err != nil {
			r.logger.Error("queueing a retry", slog.String("job", job.ID), slog.String("error", err.Error()))
			return
		}
		if failed > 0 {
			r.publish(job, Queued)
		}
		return
	}

	r.end(ctx, job, Failed, log.mask(cause.Error()))
}

// end records the state a job ended in.
func (r *Runner) end(ctx context.Context, job Job, state State, why string) {
	count, err := r.store.Queries().EndJob(ctx, store.EndJobParams{
		State: string(state),
		Error: why,
		ID:    job.ID,
	})
	if err != nil {
		r.logger.Error("recording an ended job", slog.String("job", job.ID), slog.String("error", err.Error()))
		return
	}
	if count == 0 {
		return
	}
	job.State, job.Err = state, why
	r.publish(job, state)
	if state == Failed && r.onFailure != nil {
		r.onFailure(ctx, job)
	}
}

// publish says a job changed.
func (r *Runner) publish(job Job, state State) {
	job.State = state
	r.publisher.Publish(eventJobChanged, job.Resource, jobEvent(job))
}

// cancel stops a running job, if this server runs it.
func (r *Runner) cancel(jobID string) {
	r.cancelMu.Lock()
	cancel := r.cancels[jobID]
	r.cancelMu.Unlock()
	if cancel != nil {
		cancel()
	}
}

// reap takes back the claims of workers that stopped answering, and gives the
// jobs back to the queue.
func (r *Runner) reap(ctx context.Context) {
	returned, err := r.store.Queries().ReturnExpiredClaims(ctx, interval(claimTimeout))
	switch {
	case err != nil:
		r.logger.Error("taking back expired claims", slog.String("error", err.Error()))
	case returned > 0:
		r.logger.Info("took back jobs whose worker stopped answering", slog.Int64("jobs", returned))
	}
}

// InterruptRunning marks the jobs a stopped server left running: start-up calls
// this before it runs anything.
func InterruptRunning(ctx context.Context, opened *store.Store) (int64, error) {
	interrupted, err := opened.Queries().InterruptRunningJobs(ctx)
	if err != nil {
		return 0, fmt.Errorf("jobs: interrupting the jobs left running: %w", err)
	}
	return interrupted, nil
}

// DeleteOlder deletes the records and lines of jobs that ended longer ago than
// the retention: ROOKERY_RETENTION_DAYS.
func DeleteOlder(ctx context.Context, opened *store.Store, age time.Duration) (int64, error) {
	deleted, err := opened.Queries().DeleteOldJobs(ctx, interval(age))
	if err != nil {
		return 0, fmt.Errorf("jobs: deleting old jobs: %w", err)
	}
	return deleted, nil
}

// retryBackoff is how long a retry waits: the kind's backoff, rising with the
// attempt number.
func retryBackoff(kind Kind, attempts int) time.Duration {
	backoff := kind.Backoff
	if backoff <= 0 {
		backoff = 10 * time.Second
	}
	for i := 1; i < attempts; i++ {
		backoff *= 2
		if backoff > time.Hour {
			return time.Hour
		}
	}
	return backoff
}

// interval is a duration as a Postgres interval.
func interval(duration time.Duration) pgtype.Interval {
	return pgtype.Interval{Microseconds: duration.Microseconds(), Valid: true}
}

// Worker is a plugin service's way to work the kinds it declared: it claims a
// job with a token that says the claim is still its own, sends lines and a
// heartbeat, and reports how the job ended. ServerService.Work arrives with the
// plugin system and calls this.
type Worker struct {
	store    *store.Store
	registry *Registry
	logger   *slog.Logger
}

// NewWorker returns the worker API for a registry.
func NewWorker(opened *store.Store, registry *Registry, logger *slog.Logger) *Worker {
	if logger == nil {
		logger = slog.New(slog.DiscardHandler)
	}
	return &Worker{store: opened, registry: registry, logger: logger}
}

// Claim is a job a worker holds.
type Claim struct {
	// ID is the job's id.
	ID string
	// Token is the claim's token: pass it with every report, and the job is
	// yours alone while the token matches.
	Token string
	Kind  string
	Args  []byte
	// Resource is the resource URI the job is about.
	Resource    string
	Attempts    int
	MaxAttempts int
}

// Claim waits up to wait for a job of one of the kinds, and returns nil when
// there is none.
func (w *Worker) Claim(ctx context.Context, kinds []string, wait time.Duration) (*Claim, error) {
	deadline := time.Now().Add(wait)
	for {
		token := id.Secret(24)
		record, err := w.store.Queries().ClaimPluginJob(ctx, store.ClaimPluginJobParams{
			Kinds: kinds,
			Claim: &token,
		})
		switch {
		case err == nil:
			job := JobOf(record)
			return &Claim{
				ID:          job.ID,
				Token:       token,
				Kind:        job.Kind,
				Args:        job.Args,
				Resource:    job.Resource,
				Attempts:    job.Attempts,
				MaxAttempts: job.MaxAttempts,
			}, nil
		case !errors.Is(err, pgx.ErrNoRows):
			return nil, fmt.Errorf("jobs: claiming a job for a plugin: %w", err)
		}

		if !time.Now().Before(deadline) {
			return nil, nil
		}
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-time.After(250 * time.Millisecond):
		}
	}
}

// Line adds a line to the job's log, masked.
func (w *Worker) Line(ctx context.Context, claim *Claim, line string) error {
	held, err := w.store.Queries().HeartbeatJob(ctx, store.HeartbeatJobParams{ID: claim.ID, Claim: &claim.Token})
	if err != nil {
		return fmt.Errorf("jobs: holding the claim on %s: %w", claim.ID, err)
	}
	if held == 0 {
		return fmt.Errorf("%w: the claim on %s is gone", access.ErrConflict, claim.ID)
	}
	log := NewLog(ctx, w.store, claim.ID, nil)
	return log.Write(line) //nolint:contextcheck // the log carries the job's context
}

// Heartbeat says the worker is still there.
func (w *Worker) Heartbeat(ctx context.Context, claim *Claim) error {
	held, err := w.store.Queries().HeartbeatJob(ctx, store.HeartbeatJobParams{ID: claim.ID, Claim: &claim.Token})
	if err != nil {
		return fmt.Errorf("jobs: holding the claim on %s: %w", claim.ID, err)
	}
	if held == 0 {
		return fmt.Errorf("%w: the claim on %s is gone", access.ErrConflict, claim.ID)
	}
	return nil
}

// Complete records a job as succeeded, with its result.
func (w *Worker) Complete(ctx context.Context, claim *Claim, result []byte) error {
	done, err := w.store.Queries().CompleteJob(ctx, store.CompleteJobParams{
		Result: nonNil(result),
		ID:     claim.ID,
		Claim:  &claim.Token,
	})
	if err != nil {
		return fmt.Errorf("jobs: completing %s: %w", claim.ID, err)
	}
	if done == 0 {
		return fmt.Errorf("%w: the claim on %s is gone", access.ErrConflict, claim.ID)
	}
	return nil
}

// Fail records a failed attempt, and queues another when the kind retries.
func (w *Worker) Fail(ctx context.Context, claim *Claim, cause error) error {
	kind, _ := w.registry.Kind(claim.Kind)
	if claim.Attempts < claim.MaxAttempts {
		queued, err := w.store.Queries().RetryJob(ctx, store.RetryJobParams{
			Error:   cause.Error(),
			ID:      claim.ID,
			Claim:   &claim.Token,
			Backoff: interval(retryBackoff(kind, claim.Attempts)),
		})
		if err != nil {
			return fmt.Errorf("jobs: queueing a retry of %s: %w", claim.ID, err)
		}
		if queued == 0 {
			return fmt.Errorf("%w: the claim on %s is gone", access.ErrConflict, claim.ID)
		}
		return nil
	}

	failed, err := w.store.Queries().FailJob(ctx, store.FailJobParams{
		Error: cause.Error(),
		ID:    claim.ID,
		Claim: &claim.Token,
	})
	if err != nil {
		return fmt.Errorf("jobs: failing %s: %w", claim.ID, err)
	}
	if failed == 0 {
		return fmt.Errorf("%w: the claim on %s is gone", access.ErrConflict, claim.ID)
	}
	return nil
}
