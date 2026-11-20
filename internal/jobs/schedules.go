package jobs

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"strings"
	"time"

	"connectrpc.com/connect"
	"github.com/jackc/pgx/v5"
	"github.com/robfig/cron/v3"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/id"
	"github.com/aiviarium/rookery/internal/store"
)

// Schedule is a job the deployment runs on a cron expression.
type Schedule struct {
	ID        string
	Name      string
	Kind      string
	Args      []byte
	Resource  string
	Cron      string
	Timezone  string
	Enabled   bool
	NextRunAt time.Time
	LastRunAt time.Time
	CreatedAt time.Time
}

// Message is a schedule as the edge API has it.
func (s Schedule) Message() *rookeryv1.Schedule {
	out := &rookeryv1.Schedule{
		Id:       s.ID,
		Name:     s.Name,
		Kind:     s.Kind,
		ArgsCbor: s.Args,
		Resource: s.Resource,
		Cron:     s.Cron,
		Timezone: s.Timezone,
		Enabled:  s.Enabled,
	}
	if !s.NextRunAt.IsZero() {
		out.NextRunAt = timestamppb.New(s.NextRunAt)
	}
	if !s.LastRunAt.IsZero() {
		out.LastRunAt = timestamppb.New(s.LastRunAt)
	}
	if !s.CreatedAt.IsZero() {
		out.CreatedAt = timestamppb.New(s.CreatedAt)
	}
	return out
}

// scheduleOf is a schedule from the database.
func scheduleOf(record store.RookerySchedules) Schedule {
	schedule := Schedule{
		ID:       record.ID,
		Name:     record.Name,
		Kind:     record.Kind,
		Args:     record.Args,
		Resource: record.Resource,
		Cron:     record.Cron,
		Timezone: record.Timezone,
		Enabled:  record.Enabled,
	}
	if record.NextRunAt != nil {
		schedule.NextRunAt = *record.NextRunAt
	}
	if record.LastRunAt != nil {
		schedule.LastRunAt = *record.LastRunAt
	}
	return schedule
}

// Schedules are the deployment's schedules.
type Schedules struct {
	store    *store.Store
	registry *Registry
	jobs     *Service
	parser   cron.Parser
	logger   *slog.Logger
}

// NewSchedules returns the schedules of a database.
func NewSchedules(opened *store.Store, registry *Registry, service *Service, logger *slog.Logger) *Schedules {
	if logger == nil {
		logger = slog.New(slog.DiscardHandler)
	}
	return &Schedules{
		store:    opened,
		registry: registry,
		jobs:     service,
		parser:   cron.NewParser(cron.Minute | cron.Hour | cron.Dom | cron.Month | cron.Dow | cron.Descriptor),
		logger:   logger,
	}
}

// parse checks a cron expression and a time zone, and returns both.
func (s *Schedules) parse(expression, timezone string) (cron.Schedule, *time.Location, error) {
	schedule, err := s.parser.Parse(strings.TrimSpace(expression))
	if err != nil {
		return nil, nil, fmt.Errorf("%w: %q is not a cron expression: %w", access.ErrInvalid, expression, err)
	}
	location, err := time.LoadLocation(valueOr(timezone, "UTC"))
	if err != nil {
		return nil, nil, fmt.Errorf("%w: %q is not a time zone", access.ErrInvalid, timezone)
	}
	return schedule, location, nil
}

// nextRun is when a schedule runs next, after from.
func nextRun(schedule cron.Schedule, location *time.Location, from time.Time) time.Time {
	return schedule.Next(from.In(location)).UTC()
}

// Create makes a schedule.
func (s *Schedules) Create(ctx context.Context, request ScheduleRequest) (Schedule, error) {
	if _, known := s.registry.Kind(request.Kind); !known {
		return Schedule{}, fmt.Errorf("%w: there is no job kind %q", access.ErrInvalid, request.Kind)
	}
	if strings.TrimSpace(request.Name) == "" {
		return Schedule{}, fmt.Errorf("%w: a schedule needs a name", access.ErrInvalid)
	}
	parsed, location, err := s.parse(request.Cron, request.Timezone)
	if err != nil {
		return Schedule{}, err
	}

	name := strings.TrimSpace(request.Name)
	timezone := valueOr(request.Timezone, "UTC")
	next := nextRun(parsed, location, time.Now())
	record, err := s.store.Queries().CreateSchedule(ctx, store.CreateScheduleParams{
		ID:        id.New(),
		Name:      name,
		Kind:      request.Kind,
		Args:      nonNil(request.Args),
		Resource:  request.Resource,
		Cron:      strings.TrimSpace(request.Cron),
		Timezone:  timezone,
		Enabled:   true,
		NextRunAt: &next,
	})
	if err != nil {
		if isUniqueViolation(err) {
			return Schedule{}, fmt.Errorf("%w: a schedule called %s is there already", access.ErrConflict, name)
		}
		return Schedule{}, fmt.Errorf("jobs: creating the schedule %s: %w", name, err)
	}
	return scheduleOf(record), nil
}

// ScheduleRequest is what a caller asks for when it makes a schedule.
type ScheduleRequest struct {
	Name     string
	Kind     string
	Args     []byte
	Resource string
	Cron     string
	Timezone string
}

// Update changes a schedule's fields, and recomputes when it runs next.
func (s *Schedules) Update(ctx context.Context, scheduleID string, change ScheduleChange) (Schedule, error) {
	current, err := s.ByID(ctx, scheduleID)
	if err != nil {
		return Schedule{}, err
	}

	next := current
	if change.Name != nil {
		next.Name = strings.TrimSpace(*change.Name)
	}
	if change.Kind != nil {
		next.Kind = *change.Kind
	}
	if change.Args != nil {
		next.Args = change.Args
	}
	if change.Resource != nil {
		next.Resource = *change.Resource
	}
	if change.Cron != nil {
		next.Cron = strings.TrimSpace(*change.Cron)
	}
	if change.Timezone != nil {
		next.Timezone = valueOr(*change.Timezone, "UTC")
	}
	if change.Enabled != nil {
		next.Enabled = *change.Enabled
	}
	if next.Name == "" {
		return Schedule{}, fmt.Errorf("%w: a schedule needs a name", access.ErrInvalid)
	}
	if _, known := s.registry.Kind(next.Kind); !known {
		return Schedule{}, fmt.Errorf("%w: there is no job kind %q", access.ErrInvalid, next.Kind)
	}

	parsed, location, err := s.parse(next.Cron, next.Timezone)
	if err != nil {
		return Schedule{}, err
	}
	var when *time.Time
	if next.Enabled {
		computed := nextRun(parsed, location, time.Now())
		when = &computed
	}

	record, err := s.store.Queries().UpdateScheduleFields(ctx, store.UpdateScheduleFieldsParams{
		Name:      next.Name,
		Kind:      next.Kind,
		Args:      nonNil(next.Args),
		Resource:  next.Resource,
		Cron:      next.Cron,
		Timezone:  next.Timezone,
		Enabled:   next.Enabled,
		NextRunAt: when,
		ID:        scheduleID,
	})
	if err != nil {
		if isUniqueViolation(err) {
			return Schedule{}, fmt.Errorf("%w: a schedule called %s is there already", access.ErrConflict, next.Name)
		}
		return Schedule{}, fmt.Errorf("jobs: updating the schedule %s: %w", scheduleID, err)
	}
	return scheduleOf(record), nil
}

// ScheduleChange is what an update changes: a nil field stays as it is.
type ScheduleChange struct {
	Name     *string
	Kind     *string
	Args     []byte
	Resource *string
	Cron     *string
	Timezone *string
	Enabled  *bool
}

// ByID returns a schedule.
func (s *Schedules) ByID(ctx context.Context, scheduleID string) (Schedule, error) {
	record, err := s.store.Queries().GetSchedule(ctx, scheduleID)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return Schedule{}, fmt.Errorf("%w: no schedule %s", access.ErrNotFound, scheduleID)
	case err != nil:
		return Schedule{}, fmt.Errorf("jobs: reading the schedule %s: %w", scheduleID, err)
	}
	return scheduleOf(record), nil
}

// List is a page of schedules.
func (s *Schedules) List(ctx context.Context, cursor string, limit int) ([]Schedule, error) {
	records, err := s.store.Queries().ListSchedules(ctx, store.ListSchedulesParams{ID: cursor, Limit: clampLimit(limit)})
	if err != nil {
		return nil, fmt.Errorf("jobs: listing schedules: %w", err)
	}
	out := make([]Schedule, 0, len(records))
	for _, record := range records {
		out = append(out, scheduleOf(record))
	}
	return out, nil
}

// Delete removes a schedule.
func (s *Schedules) Delete(ctx context.Context, scheduleID string) error {
	deleted, err := s.store.Queries().DeleteSchedule(ctx, scheduleID)
	switch {
	case err != nil:
		return fmt.Errorf("jobs: deleting the schedule %s: %w", scheduleID, err)
	case deleted == 0:
		return fmt.Errorf("%w: no schedule %s", access.ErrNotFound, scheduleID)
	}
	return nil
}

// RunNow enqueues a schedule's job at once, without moving the schedule's clock.
func (s *Schedules) RunNow(ctx context.Context, scheduleID string) (Job, error) {
	schedule, err := s.ByID(ctx, scheduleID)
	if err != nil {
		return Job{}, err
	}
	job, err := s.jobs.EnqueueSystem(ctx, EnqueueRequest{
		Kind:     schedule.Kind,
		Args:     schedule.Args,
		Resource: schedule.Resource,
	})
	if err != nil {
		return Job{}, err
	}
	var next *time.Time
	if !schedule.NextRunAt.IsZero() {
		next = &schedule.NextRunAt
	}
	if err := s.store.Queries().SetScheduleNextRun(ctx, store.SetScheduleNextRunParams{
		ID:        schedule.ID,
		NextRunAt: next,
	}); err != nil {
		return Job{}, fmt.Errorf("jobs: recording the run of %s: %w", schedule.ID, err)
	}
	return job, nil
}

// Scheduler runs the schedules that are due. It is one loop on the leader.
type Scheduler struct {
	schedules *Schedules
	interval  time.Duration
}

// NewScheduler returns a scheduler for a set of schedules.
func NewScheduler(schedules *Schedules) *Scheduler {
	return &Scheduler{schedules: schedules, interval: 30 * time.Second}
}

// Run runs the due schedules until ctx ends. A run that was missed while the
// server was not the leader is enqueued once, and the schedule carries on from
// now.
func (s *Scheduler) Run(ctx context.Context) error {
	ticker := time.NewTicker(s.interval)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
			if err := s.runDue(ctx); err != nil && ctx.Err() == nil {
				s.schedules.logger.Error("running the due schedules", slog.String("error", err.Error()))
			}
		}
	}
}

// runDue enqueues the jobs of the schedules that are due.
func (s *Scheduler) runDue(ctx context.Context) error {
	due, err := s.schedules.store.Queries().ListDueSchedules(ctx, 100)
	if err != nil {
		return fmt.Errorf("jobs: reading the due schedules: %w", err)
	}
	for _, record := range due {
		schedule := scheduleOf(record)
		if _, err := s.schedules.jobs.EnqueueSystem(ctx, EnqueueRequest{
			Kind:     schedule.Kind,
			Args:     schedule.Args,
			Resource: schedule.Resource,
		}); err != nil {
			s.schedules.logger.Error("enqueueing a schedule's job",
				slog.String("schedule", schedule.Name), slog.String("error", err.Error()))
			continue
		}

		var next time.Time
		if parsed, location, err := s.schedules.parse(schedule.Cron, schedule.Timezone); err == nil {
			next = nextRun(parsed, location, time.Now())
		}
		if err := s.schedules.store.Queries().UpdateScheduleRun(ctx, store.UpdateScheduleRunParams{
			ID:        schedule.ID,
			NextRunAt: &next,
		}); err != nil {
			return fmt.Errorf("jobs: recording the run of %s: %w", schedule.ID, err)
		}
	}
	return nil
}

// SchedulesService is ScheduleService.
type SchedulesService struct {
	schedules *Schedules
}

// NewSchedulesService returns ScheduleService for a database.
func NewSchedulesService(schedules *Schedules) *SchedulesService {
	return &SchedulesService{schedules: schedules}
}

// List returns a page of schedules.
func (s *SchedulesService) List(ctx context.Context, request *connect.Request[rookeryv1.ScheduleServiceListRequest]) (*connect.Response[rookeryv1.ScheduleServiceListResponse], error) {
	found, err := s.schedules.List(ctx, request.Msg.GetCursor(), int(request.Msg.GetLimit()))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := make([]*rookeryv1.Schedule, 0, len(found))
	for _, schedule := range found {
		out = append(out, schedule.Message())
	}
	next := ""
	limit := int(request.Msg.GetLimit())
	if limit <= 0 {
		limit = 100
	}
	if len(found) == limit {
		next = found[len(found)-1].ID
	}
	return connect.NewResponse(&rookeryv1.ScheduleServiceListResponse{Schedules: out, NextCursor: next}), nil
}

// Create makes a schedule.
func (s *SchedulesService) Create(ctx context.Context, request *connect.Request[rookeryv1.ScheduleServiceCreateRequest]) (*connect.Response[rookeryv1.ScheduleServiceCreateResponse], error) {
	schedule, err := s.schedules.Create(ctx, ScheduleRequest{
		Name:     request.Msg.GetName(),
		Kind:     request.Msg.GetKind(),
		Args:     request.Msg.GetArgsCbor(),
		Resource: request.Msg.GetResource(),
		Cron:     request.Msg.GetCron(),
		Timezone: request.Msg.GetTimezone(),
	})
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ScheduleServiceCreateResponse{Schedule: schedule.Message()}), nil
}

// Update changes a schedule.
func (s *SchedulesService) Update(ctx context.Context, request *connect.Request[rookeryv1.ScheduleServiceUpdateRequest]) (*connect.Response[rookeryv1.ScheduleServiceUpdateResponse], error) {
	change := ScheduleChange{
		Args: request.Msg.ArgsCbor,
	}
	if request.Msg.Name != nil {
		change.Name = request.Msg.Name
	}
	if request.Msg.Kind != nil {
		change.Kind = request.Msg.Kind
	}
	if request.Msg.Resource != nil {
		change.Resource = request.Msg.Resource
	}
	if request.Msg.Cron != nil {
		change.Cron = request.Msg.Cron
	}
	if request.Msg.Timezone != nil {
		change.Timezone = request.Msg.Timezone
	}
	if request.Msg.Enabled != nil {
		change.Enabled = request.Msg.Enabled
	}
	schedule, err := s.schedules.Update(ctx, request.Msg.GetSchedule(), change)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ScheduleServiceUpdateResponse{Schedule: schedule.Message()}), nil
}

// Delete removes a schedule.
func (s *SchedulesService) Delete(ctx context.Context, request *connect.Request[rookeryv1.ScheduleServiceDeleteRequest]) (*connect.Response[rookeryv1.ScheduleServiceDeleteResponse], error) {
	if err := s.schedules.Delete(ctx, request.Msg.GetSchedule()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ScheduleServiceDeleteResponse{}), nil
}

// RunNow enqueues a schedule's job at once.
func (s *SchedulesService) RunNow(ctx context.Context, request *connect.Request[rookeryv1.ScheduleServiceRunNowRequest]) (*connect.Response[rookeryv1.ScheduleServiceRunNowResponse], error) {
	job, err := s.schedules.RunNow(ctx, request.Msg.GetSchedule())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ScheduleServiceRunNowResponse{Job: job.ID}), nil
}

// valueOr is a value, or a fallback when it is empty.
func valueOr(value, fallback string) string {
	if strings.TrimSpace(value) == "" {
		return fallback
	}
	return value
}

// isUniqueViolation reports a Postgres unique constraint error.
func isUniqueViolation(err error) bool {
	var pgErr interface{ SQLState() string }
	return errors.As(err, &pgErr) && pgErr.SQLState() == "23505"
}
