package rollouts

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"sort"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/id"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/messaging"
	"github.com/aiviarium/rookery/internal/store"
)

// Options are what a roll-out service is made of.
type Options struct {
	// Store is the database.
	Store *store.Store
	// Checker answers what the caller holds.
	Checker *access.Checker
	// Jobs enqueues and cancels the child jobs.
	Jobs *jobs.Service
	// Hosts is the host registry, without which only hosts named one by one
	// can be selected.
	Hosts HostSelector
	// Notifications sends the creator of a failed or paused roll-out a word.
	Notifications *messaging.Notifications
	// Publisher says a roll-out changed, for the servers watching.
	Publisher jobs.Publisher
	// Logger is where the advance loop says what it did.
	Logger *slog.Logger
}

// Service plans, starts and steers roll-outs.
type Service struct {
	store         *store.Store
	checker       *access.Checker
	jobs          *jobs.Service
	hosts         HostSelector
	notifications *messaging.Notifications
	publisher     jobs.Publisher
	logger        *slog.Logger
}

// New returns a roll-out service.
func New(options Options) *Service {
	logger := options.Logger
	if logger == nil {
		logger = slog.New(slog.DiscardHandler)
	}
	publisher := options.Publisher
	if publisher == nil {
		publisher = quietPublisher{}
	}
	return &Service{
		store:         options.Store,
		checker:       options.Checker,
		jobs:          options.Jobs,
		hosts:         options.Hosts,
		notifications: options.Notifications,
		publisher:     publisher,
		logger:        logger,
	}
}

// Plan is what a selection resolves to.
type Plan struct {
	// Hosts are the hosts the roll-out would work.
	Hosts []Host
	// Skipped are the hosts the selection named that cannot take it, with the
	// reason.
	Skipped []Host
}

// StartRequest is what a caller asks a roll-out to do. It is also what a
// schedule that starts a roll-out holds, as CBOR.
type StartRequest struct {
	// Kind is host.apply or host.action.
	Kind string `cbor:"kind" json:"kind"`
	// Args are the child job's arguments, such as which action.
	Args []byte `cbor:"args,omitempty" json:"args,omitempty"`
	// Resource is the resource URI the roll-out is about.
	Resource string `cbor:"resource,omitempty" json:"resource,omitempty"`
	// Selection says which hosts it is for.
	Selection Selection `cbor:"selection" json:"selection"`
	// Strategy says how it runs.
	Strategy Strategy `cbor:"strategy,omitempty" json:"strategy,omitempty"`
}

// StartScheduled starts the roll-out a schedule's CBOR spec asks for. The
// spec is a StartRequest, and the one who made the schedule had to hold manage
// on the resource it names.
func (s *Service) StartScheduled(ctx context.Context, spec []byte) (string, error) {
	var request StartRequest
	if err := decb(spec, &request); err != nil {
		return "", err
	}
	rollout, err := s.StartSystem(ctx, request)
	if err != nil {
		return "", err
	}
	return rollout.ID, nil
}

// Plan resolves a selection into the hosts a roll-out would work.
func (s *Service) Plan(ctx context.Context, kind string, selection Selection) (Plan, error) {
	if err := checkKind(kind); err != nil {
		return Plan{}, err
	}
	if err := selection.Validate(); err != nil {
		return Plan{}, err
	}
	hosts, skipped, err := s.resolve(ctx, selection)
	if err != nil {
		return Plan{}, err
	}
	return Plan{Hosts: hosts, Skipped: skipped}, nil
}

// Start makes a roll-out, records its hosts, and starts its first batch.
func (s *Service) Start(ctx context.Context, request StartRequest) (Rollout, error) {
	principal, ok := access.PrincipalFrom(ctx)
	if !ok {
		return Rollout{}, access.ErrUnauthenticated
	}
	return s.start(ctx, request, principal)
}

// StartSystem starts a roll-out with no caller behind it: a schedule's run.
func (s *Service) StartSystem(ctx context.Context, request StartRequest) (Rollout, error) {
	return s.start(ctx, request, access.LocalAdmin())
}

// start is Start for a caller the caller has already resolved.
func (s *Service) start(ctx context.Context, request StartRequest, principal access.Principal) (Rollout, error) {
	if err := checkKind(request.Kind); err != nil {
		return Rollout{}, err
	}
	if err := request.Selection.Validate(); err != nil {
		return Rollout{}, err
	}
	if err := request.Strategy.Validate(); err != nil {
		return Rollout{}, err
	}

	resource := strings.TrimSpace(request.Resource)
	if resource != "" {
		if _, err := access.ParseResource(resource); err != nil {
			return Rollout{}, err
		}
	}
	if err := s.checker.Check(ctx, principal, resource, access.Use); err != nil {
		return Rollout{}, err
	}

	resolved, err := s.Plan(ctx, request.Kind, request.Selection)
	if err != nil {
		return Rollout{}, err
	}
	if len(resolved.Hosts) == 0 {
		return Rollout{}, fmt.Errorf("%w: the selection names no host that can take a %s", access.ErrInvalid, request.Kind)
	}

	strategy := request.Strategy.Normalize()
	hosts := assignBatches(resolved.Hosts, strategy)

	selectionBytes, err := cborx.Marshal(request.Selection)
	if err != nil {
		return Rollout{}, fmt.Errorf("rollouts: recording the selection: %w", err)
	}
	strategyBytes, err := cborx.Marshal(strategy)
	if err != nil {
		return Rollout{}, fmt.Errorf("rollouts: recording the strategy: %w", err)
	}

	rolloutID := id.New()
	tx, err := s.store.Pool().Begin(ctx)
	if err != nil {
		return Rollout{}, fmt.Errorf("rollouts: beginning the start: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	queries := s.store.Queries().WithTx(tx)
	record, err := queries.CreateRollout(ctx, store.CreateRolloutParams{
		ID:        rolloutID,
		Kind:      request.Kind,
		Args:      nonNil(request.Args),
		Resource:  resource,
		CreatedBy: principal.String(),
		State:     string(Running),
		Selection: selectionBytes,
		Strategy:  strategyBytes,
	})
	if err != nil {
		return Rollout{}, fmt.Errorf("rollouts: recording the roll-out: %w", err)
	}
	for _, host := range hosts {
		if err := queries.CreateRolloutHost(ctx, store.CreateRolloutHostParams{
			RolloutID: rolloutID,
			Host:      host.Name,
			Batch:     int32(host.Batch), //nolint:gosec // a batch number is a host count
			State:     string(HostPending),
		}); err != nil {
			return Rollout{}, fmt.Errorf("rollouts: recording the host %s: %w", host.Name, err)
		}
	}

	// The roll-out is a resource of its own, under the resource it is about, so
	// that a grant on that resource reaches it. Its caller manages it.
	if resource != "" {
		if err := queries.UpsertResource(ctx, store.UpsertResourceParams{Uri: resource}); err != nil {
			return Rollout{}, fmt.Errorf("rollouts: recording the resource %s: %w", resource, err)
		}
	}
	uri := access.RolloutURI(rolloutID)
	if err := queries.UpsertResource(ctx, store.UpsertResourceParams{Uri: uri, Parent: optional(resource)}); err != nil {
		return Rollout{}, fmt.Errorf("rollouts: recording the roll-out's resource: %w", err)
	}
	if principal.Kind == access.KindUser {
		if _, err := queries.UpsertGrant(ctx, store.UpsertGrantParams{
			Resource:  uri,
			Principal: access.UserPrincipal(principal.ID),
			Level:     int16(access.Manage),
		}); err != nil {
			return Rollout{}, fmt.Errorf("rollouts: granting the caller manage on the roll-out: %w", err)
		}
	}

	if err := tx.Commit(ctx); err != nil {
		return Rollout{}, fmt.Errorf("rollouts: committing the start: %w", err)
	}

	rollout, err := rolloutOf(record)
	if err != nil {
		return Rollout{}, err
	}
	rollout.Hosts = hosts
	rollout.Skipped = resolved.Skipped
	s.publish(rollout)

	// The first batch starts now, so a roll-out with one host is under way
	// before the caller reads it back.
	if err := s.advanceOne(ctx, rolloutID); err != nil {
		s.logger.Error("starting the first batch", slog.String("rollout", rolloutID), slog.String("error", err.Error()))
	}
	return s.Get(ctx, rolloutID)
}

// List is a page of roll-outs, newest first.
func (s *Service) List(ctx context.Context, state, cursor string, limit int) ([]Rollout, error) {
	if cursor == "" {
		// Ids are ULIDs in Crockford's base32, so "~" sorts after all of them.
		cursor = "~"
	}
	records, err := s.store.Queries().ListRollouts(ctx, store.ListRolloutsParams{
		ID:    cursor,
		State: optional(state),
		Limit: clampLimit(limit),
	})
	if err != nil {
		return nil, fmt.Errorf("rollouts: listing roll-outs: %w", err)
	}

	out := make([]Rollout, 0, len(records))
	ids := make([]string, 0, len(records))
	for _, record := range records {
		rollout, err := rolloutOf(record)
		if err != nil {
			return nil, err
		}
		out = append(out, rollout)
		ids = append(ids, rollout.ID)
	}
	if len(ids) == 0 {
		return out, nil
	}

	hostRows, err := s.store.Queries().ListRolloutHostsIn(ctx, ids)
	if err != nil {
		return nil, fmt.Errorf("rollouts: reading the hosts of the roll-outs: %w", err)
	}
	byRollout := make(map[string][]Host, len(ids))
	for _, row := range hostRows {
		host := hostOf(row)
		if host.State == HostSkipped {
			continue
		}
		byRollout[row.RolloutID] = append(byRollout[row.RolloutID], host)
	}
	for i := range out {
		out[i].Hosts = byRollout[out[i].ID]
	}
	return out, nil
}

// Get returns a roll-out and its hosts.
func (s *Service) Get(ctx context.Context, rolloutID string) (Rollout, error) {
	record, err := s.store.Queries().GetRollout(ctx, rolloutID)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return Rollout{}, fmt.Errorf("%w: no roll-out %s", access.ErrNotFound, rolloutID)
	case err != nil:
		return Rollout{}, fmt.Errorf("rollouts: reading the roll-out %s: %w", rolloutID, err)
	}
	rollout, err := rolloutOf(record)
	if err != nil {
		return Rollout{}, err
	}

	rows, err := s.store.Queries().ListRolloutHosts(ctx, rolloutID)
	if err != nil {
		return Rollout{}, fmt.Errorf("rollouts: reading the hosts of %s: %w", rolloutID, err)
	}
	for _, row := range rows {
		host := hostOf(row)
		if host.State == HostSkipped {
			rollout.Skipped = append(rollout.Skipped, host)
			continue
		}
		rollout.Hosts = append(rollout.Hosts, host)
	}
	return rollout, nil
}

// Pause stops a roll-out: the jobs that run finish, and no more start.
func (s *Service) Pause(ctx context.Context, rolloutID, why string) (Rollout, error) {
	rollout, err := s.Get(ctx, rolloutID)
	if err != nil {
		return Rollout{}, err
	}
	switch rollout.State {
	case Paused:
		return rollout, nil
	case Succeeded, Failed, Cancelled:
		return Rollout{}, fmt.Errorf("%w: the roll-out %s has ended", access.ErrConflict, rolloutID)
	}
	if _, err := s.store.Queries().PauseRollout(ctx, store.PauseRolloutParams{ID: rolloutID, PausedBecause: why}); err != nil {
		return Rollout{}, fmt.Errorf("rollouts: pausing %s: %w", rolloutID, err)
	}
	paused, err := s.Get(ctx, rolloutID)
	if err != nil {
		return Rollout{}, err
	}
	s.publish(paused)
	s.notifyPaused(ctx, paused, why)
	return paused, nil
}

// Resume lets a paused roll-out carry on.
func (s *Service) Resume(ctx context.Context, rolloutID string) (Rollout, error) {
	rollout, err := s.Get(ctx, rolloutID)
	if err != nil {
		return Rollout{}, err
	}
	switch rollout.State {
	case Paused:
	case Succeeded, Failed, Cancelled:
		return Rollout{}, fmt.Errorf("%w: the roll-out %s has ended", access.ErrConflict, rolloutID)
	default:
		return Rollout{}, fmt.Errorf("%w: the roll-out %s is not paused", access.ErrConflict, rolloutID)
	}
	if _, err := s.store.Queries().ResumeRollout(ctx, rolloutID); err != nil {
		return Rollout{}, fmt.Errorf("rollouts: resuming %s: %w", rolloutID, err)
	}
	resumed, err := s.Get(ctx, rolloutID)
	if err != nil {
		return Rollout{}, err
	}
	s.publish(resumed)
	if resumed.Hosts == nil && resumed.State == Running {
		// Nothing more to do: the advance loop starts the next batch.
		return resumed, nil
	}
	return resumed, nil
}

// Cancel stops a roll-out, cancelling the jobs that are running.
func (s *Service) Cancel(ctx context.Context, rolloutID string) (Rollout, error) {
	rollout, err := s.Get(ctx, rolloutID)
	if err != nil {
		return Rollout{}, err
	}
	if rollout.State.Done() {
		return Rollout{}, fmt.Errorf("%w: the roll-out %s has ended", access.ErrConflict, rolloutID)
	}

	for _, host := range rollout.Hosts {
		if host.State != HostRunning {
			continue
		}
		if host.Job != "" {
			if _, err := s.jobs.Cancel(ctx, host.Job); err != nil {
				s.logger.Warn("cancelling a host's job",
					slog.String("rollout", rolloutID), slog.String("host", host.Name), slog.String("error", err.Error()))
			}
		}
		if _, err := s.store.Queries().EndRolloutHost(ctx, store.EndRolloutHostParams{
			RolloutID: rolloutID,
			State:     string(HostCancelled),
			Host:      host.Name,
		}); err != nil {
			return Rollout{}, fmt.Errorf("rollouts: cancelling the host %s: %w", host.Name, err)
		}
	}
	if _, err := s.store.Queries().CancelPendingRolloutHosts(ctx, rolloutID); err != nil {
		return Rollout{}, fmt.Errorf("rollouts: cancelling the hosts of %s: %w", rolloutID, err)
	}
	if _, err := s.store.Queries().FinishRollout(ctx, store.FinishRolloutParams{ID: rolloutID, State: string(Cancelled)}); err != nil {
		return Rollout{}, fmt.Errorf("rollouts: cancelling %s: %w", rolloutID, err)
	}

	cancelled, err := s.Get(ctx, rolloutID)
	if err != nil {
		return Rollout{}, err
	}
	s.publish(cancelled)
	return cancelled, nil
}

// Watch calls send with the roll-out whenever it or one of its hosts changes,
// and returns when it has ended or ctx does.
func (s *Service) Watch(ctx context.Context, rolloutID string, send func(Rollout) error) error {
	// A roll-out that is not there is an error before the stream starts, so a
	// caller sees NotFound rather than an empty stream.
	if _, err := s.Get(ctx, rolloutID); err != nil {
		return err
	}

	ticker := time.NewTicker(watchInterval)
	defer ticker.Stop()

	last := ""
	for {
		rollout, err := s.Get(ctx, rolloutID)
		if err != nil {
			return err
		}
		if fingerprint := signature(rollout); fingerprint != last {
			last = fingerprint
			if err := send(rollout); err != nil {
				return err
			}
		}
		if rollout.State.Done() {
			return nil
		}

		select {
		case <-ctx.Done():
			return nil
		case <-ticker.C:
		}
	}
}

// resolve is a selection as the hosts it names, and the ones it cannot use.
func (s *Service) resolve(ctx context.Context, selection Selection) ([]Host, []Host, error) {
	selector, err := ParseSelector(selection.Labels)
	if err != nil {
		return nil, nil, err
	}

	candidates, err := s.candidates(ctx, selection)
	if err != nil {
		return nil, nil, err
	}

	var hosts, skipped []Host
	for _, candidate := range candidates {
		if !matches(candidate, selection, selector) {
			continue
		}
		if candidate.Skip != "" {
			skipped = append(skipped, Host{Name: candidate.Name, State: HostSkipped, Reason: candidate.Skip})
			continue
		}
		hosts = append(hosts, Host{Name: candidate.Name, State: HostPending})
	}
	sortHosts(hosts)
	sortHosts(skipped)
	return hosts, skipped, nil
}

// candidates are the hosts a selection could name. Hosts named one by one need
// no host registry; labels and the hosts that are behind do.
func (s *Service) candidates(ctx context.Context, selection Selection) ([]Candidate, error) {
	if selection.Labels == "" && !selection.Behind {
		out := make([]Candidate, 0, len(selection.Hosts))
		for _, host := range selection.Hosts {
			out = append(out, Candidate{Name: host})
		}
		return out, nil
	}
	if s.hosts == nil {
		return nil, fmt.Errorf("%w: this server has no host registry, so a roll-out selects hosts by name", access.ErrUnavailable)
	}
	hosts, err := s.hosts.Hosts(ctx)
	if err != nil {
		return nil, fmt.Errorf("rollouts: reading the hosts: %w", err)
	}
	return hosts, nil
}

// advanceOne moves a roll-out on: it reads how its running hosts ended, then
// starts the next batch, pauses it, or ends it.
func (s *Service) advanceOne(ctx context.Context, rolloutID string) error {
	rollout, err := s.Get(ctx, rolloutID)
	if err != nil {
		return err
	}
	if rollout.State != Running {
		return nil
	}

	// A host's job is what says how the host did.
	for _, host := range rollout.Hosts {
		if host.State != HostRunning {
			continue
		}
		state, why := s.hostOutcome(ctx, host)
		if state == "" {
			continue
		}
		if _, err := s.store.Queries().EndRolloutHost(ctx, store.EndRolloutHostParams{
			RolloutID: rolloutID,
			State:     string(state),
			Error:     why,
			Host:      host.Name,
		}); err != nil {
			return fmt.Errorf("rollouts: recording the host %s: %w", host.Name, err)
		}
	}

	rollout, err = s.Get(ctx, rolloutID)
	if err != nil {
		return err
	}
	counts := rollout.Counts()

	// The hosts that failed stop the roll-out once there are enough of them.
	if counts.Failed >= rollout.Strategy.Normalize().FailuresAllowed {
		why := fmt.Sprintf("%d %s failed", counts.Failed, hosts(counts.Failed))
		return s.pause(ctx, rollout, why)
	}
	if counts.Running > 0 {
		return nil
	}

	if counts.Pending == 0 {
		state := Succeeded
		if counts.Failed > 0 {
			state = Failed
		}
		return s.finish(ctx, rollout, state)
	}

	// The canary is a batch of its own, and the roll-out waits for Resume once
	// it has run.
	if canary := rollout.Strategy.Normalize().Canary; canary > 0 && !rollout.CanaryDone && canaryDone(rollout) {
		return s.pause(ctx, rollout, "the canary finished; resume to run the rest")
	}

	// A pause between batches: it is over when its time has come.
	if pause := rollout.Strategy.Normalize().Pause; pause > 0 && rollout.PausedUntil.IsZero() {
		until := time.Now().Add(pause)
		updated, err := s.store.Queries().SetRolloutPause(ctx, store.SetRolloutPauseParams{ID: rolloutID, PausedUntil: &until})
		if err != nil {
			return fmt.Errorf("rollouts: waiting between the batches of %s: %w", rolloutID, err)
		}
		waiting, err := rolloutOf(updated)
		if err != nil {
			return err
		}
		s.publish(waiting)
		return nil
	}

	next, err := s.store.Queries().NextRolloutBatch(ctx, rolloutID)
	if err != nil {
		return fmt.Errorf("rollouts: reading the next batch of %s: %w", rolloutID, err)
	}
	for _, row := range next {
		if err := s.startHost(ctx, rollout, hostOf(row)); err != nil {
			return err
		}
	}
	if _, err := s.store.Queries().SetRolloutPause(ctx, store.SetRolloutPauseParams{ID: rolloutID}); err != nil {
		return fmt.Errorf("rollouts: clearing the pause of %s: %w", rolloutID, err)
	}
	s.publish(rollout)
	return nil
}

// startHost enqueues a host's child job and records it as running.
func (s *Service) startHost(ctx context.Context, rollout Rollout, host Host) error {
	job, err := s.jobs.EnqueueSystem(ctx, jobs.EnqueueRequest{
		Kind:     rollout.Kind,
		Args:     rollout.Args,
		Resource: access.HostURI(host.Name),
		Parent:   rollout.ID,
	})
	if err != nil {
		// A host whose job cannot be enqueued has failed, so the roll-out
		// carries on rather than stopping here.
		s.logger.Error("enqueueing a host's job",
			slog.String("rollout", rollout.ID), slog.String("host", host.Name), slog.String("error", err.Error()))
		_, endErr := s.store.Queries().EndRolloutHost(ctx, store.EndRolloutHostParams{
			RolloutID: rollout.ID,
			State:     string(HostFailed),
			Error:     err.Error(),
			Host:      host.Name,
		})
		return endErr
	}
	if _, err := s.store.Queries().StartRolloutHost(ctx, store.StartRolloutHostParams{
		RolloutID: rollout.ID,
		Job:       job.ID,
		Host:      host.Name,
	}); err != nil {
		return fmt.Errorf("rollouts: starting the host %s: %w", host.Name, err)
	}
	return nil
}

// hostOutcome is how a running host ended, or empty while its job runs.
func (s *Service) hostOutcome(ctx context.Context, host Host) (HostState, string) {
	if host.Job == "" {
		return HostFailed, "the host got no job"
	}
	job, err := s.jobs.Get(ctx, host.Job)
	switch {
	case errors.Is(err, access.ErrNotFound):
		return HostFailed, "its job is gone"
	case err != nil:
		s.logger.Error("reading a host's job", slog.String("job", host.Job), slog.String("error", err.Error()))
		return "", ""
	}
	switch job.State {
	case jobs.Succeeded:
		return HostSucceeded, ""
	case jobs.Failed, jobs.Cancelled, jobs.Interrupted:
		why := job.Err
		if why == "" {
			why = "the job ended " + string(job.State)
		}
		return HostFailed, why
	default:
		return "", ""
	}
}

// pause stops a roll-out and tells the caller who started it.
func (s *Service) pause(ctx context.Context, rollout Rollout, why string) error {
	paused, err := s.store.Queries().PauseRollout(ctx, store.PauseRolloutParams{ID: rollout.ID, PausedBecause: why})
	if err != nil {
		return fmt.Errorf("rollouts: pausing %s: %w", rollout.ID, err)
	}
	stopped, err := rolloutOf(paused)
	if err != nil {
		return err
	}
	s.publish(stopped)
	s.notifyPaused(ctx, stopped, why)
	return nil
}

// finish ends a roll-out.
func (s *Service) finish(ctx context.Context, rollout Rollout, state State) error {
	finished, err := s.store.Queries().FinishRollout(ctx, store.FinishRolloutParams{ID: rollout.ID, State: string(state)})
	if err != nil {
		return fmt.Errorf("rollouts: ending %s: %w", rollout.ID, err)
	}
	ended, err := rolloutOf(finished)
	if err != nil {
		return err
	}
	s.publish(ended)
	if state == Failed {
		s.notify(ctx, ended, "Roll-out failed", fmt.Sprintf("The roll-out of %s on %s failed.", ended.Kind, plural(len(ended.Hosts), "host")))
	}
	return nil
}

// notifyPaused tells the creator that a roll-out stopped and how to carry on.
func (s *Service) notifyPaused(ctx context.Context, rollout Rollout, why string) {
	s.notify(ctx, rollout, "Roll-out paused", fmt.Sprintf("The roll-out of %s stopped: %s. Resume it on its page.", rollout.Kind, why))
}

// notify sends a roll-out's creator a notification, when a user started it.
func (s *Service) notify(ctx context.Context, rollout Rollout, title, body string) {
	if s.notifications == nil || rollout.CreatedBy == "" {
		return
	}
	user, ok := userOf(rollout.CreatedBy)
	if !ok {
		return
	}
	if _, err := s.notifications.Send(ctx, messaging.Notification{
		UserID:   user,
		Title:    title,
		Body:     body,
		Link:     "/rollouts/" + rollout.ID,
		Resource: access.RolloutURI(rollout.ID),
	}); err != nil {
		s.logger.Error("telling the creator of a roll-out", slog.String("rollout", rollout.ID), slog.String("error", err.Error()))
	}
}

// publish says a roll-out changed, for the servers watching.
func (s *Service) publish(rollout Rollout) {
	payload, err := cborx.Marshal(map[string]any{"id": rollout.ID, "state": string(rollout.State)})
	if err != nil {
		return
	}
	s.publisher.Publish(eventChanged, rollout.Resource, payload)
}

// eventChanged is the core event for a roll-out changing.
const eventChanged = "rookery/rollouts.changed"

// watchInterval is how often Watch looks again.
const watchInterval = 300 * time.Millisecond

// quietPublisher is a Publisher that tells nobody.
type quietPublisher struct{}

// Publish does nothing.
func (quietPublisher) Publish(string, string, []byte) {}

// checkKind checks that a roll-out runs work a roll-out can run.
func checkKind(kind string) error {
	switch kind {
	case KindApply, KindAction:
		return nil
	default:
		return fmt.Errorf("%w: a roll-out runs %s or %s, not %q", access.ErrInvalid, KindApply, KindAction, kind)
	}
}

// assignBatches numbers the hosts' batches: the canary first, then batches of
// the strategy's size.
func assignBatches(hosts []Host, strategy Strategy) []Host {
	size := strategy.batchSize(len(hosts))
	canary := min(strategy.Canary, len(hosts))
	for i := range hosts {
		switch {
		case i < canary:
			hosts[i].Batch = 0
		case canary == 0:
			hosts[i].Batch = i / size
		default:
			hosts[i].Batch = 1 + (i-canary)/size
		}
	}
	return hosts
}

// canaryDone reports whether no host of the canary batch is still pending.
func canaryDone(rollout Rollout) bool {
	for _, host := range rollout.Hosts {
		if host.Batch == 0 && host.State == HostPending {
			return false
		}
	}
	return true
}

// sortHosts orders hosts by name, so that a plan reads the same every time.
func sortHosts(hosts []Host) {
	sort.Slice(hosts, func(i, j int) bool { return hosts[i].Name < hosts[j].Name })
}

// signature is what Watch compares to see that a roll-out changed.
func signature(rollout Rollout) string {
	var b strings.Builder
	b.WriteString(string(rollout.State))
	b.WriteString("|")
	b.WriteString(rollout.PausedBecause)
	for _, host := range rollout.Hosts {
		b.WriteString("|")
		b.WriteString(host.Name)
		b.WriteString(":")
		b.WriteString(string(host.State))
		b.WriteString(":")
		b.WriteString(host.Job)
	}
	return b.String()
}

// userOf reads the user out of a creator principal.
func userOf(principal string) (string, bool) {
	kind, id, found := strings.Cut(principal, ":")
	if !found || kind != access.KindUser || id == "" {
		return "", false
	}
	return id, true
}

// hosts is "host" or "hosts".
func hosts(count int) string { return plural(count, "host") }

// plural is a count and a word, with an s when there is more than one.
func plural(count int, word string) string {
	if count == 1 {
		return fmt.Sprintf("%d %s", count, word)
	}
	return fmt.Sprintf("%d %ss", count, word)
}

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
