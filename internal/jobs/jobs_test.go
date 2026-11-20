package jobs_test

import (
	"context"
	"fmt"
	"net/http"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/access/accesstest"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// harness is a test's jobs: the runner working, the service in front.
type harness struct {
	store    *store.Store
	client   rookeryv1connect.JobServiceClient
	registry *jobs.Registry
	service  *jobs.Service
}

// startJobs runs a runner for a registry and serves JobService for a caller.
func startJobs(t *testing.T, registered func(registry *jobs.Registry)) *harness {
	t.Helper()

	opened := storetest.New(t)
	checker := access.NewChecker(opened)
	registry := jobs.NewRegistry()
	if err := registry.RegisterKinds(jobs.CoreKinds()...); err != nil {
		t.Fatalf("registering the core kinds: %v", err)
	}
	registered(registry)

	runner := jobs.NewRunner(opened, registry, nil, nil)
	service := jobs.NewService(opened, checker, registry, runner, nil)

	runCtx, cancel := context.WithCancel(t.Context())
	done := make(chan error, 1)
	go func() { done <- runner.Run(runCtx) }()
	t.Cleanup(func() {
		cancel()
		select {
		case err := <-done:
			_ = err
		case <-time.After(10 * time.Second):
			t.Error("the runner did not stop")
		}
	})

	server := accesstest.New(t, checker, accesstest.User("01hadmin", true), func(mux *http.ServeMux, options []connect.HandlerOption) {
		path, handler := rookeryv1connect.NewJobServiceHandler(jobs.NewJobService(service), options...)
		mux.Handle(path, handler)
	})
	return &harness{
		store:    opened,
		client:   rookeryv1connect.NewJobServiceClient(server.Client(), server.URL, connect.WithGRPC()),
		registry: registry,
		service:  service,
	}
}

// enqueue makes a job and fails the test when it cannot.
func (h *harness) enqueue(t *testing.T, request *rookeryv1.JobServiceEnqueueRequest) *rookeryv1.Job {
	t.Helper()

	made, err := h.client.Enqueue(t.Context(), connect.NewRequest(request))
	if err != nil {
		t.Fatalf("enqueueing %s: %v", request.GetKind(), err)
	}
	return made.Msg.GetJob()
}

// waitForState waits until a job is in a state.
func (h *harness) waitForState(t *testing.T, jobID string, want ...jobs.State) *rookeryv1.Job {
	t.Helper()

	deadline := time.Now().Add(15 * time.Second)
	var last string
	for time.Now().Before(deadline) {
		found, err := h.client.Get(t.Context(), connect.NewRequest(&rookeryv1.JobServiceGetRequest{Job: jobID}))
		if err != nil {
			t.Fatalf("reading %s: %v", jobID, err)
		}
		last = found.Msg.GetJob().GetState()
		for _, state := range want {
			if last == string(state) {
				return found.Msg.GetJob()
			}
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("the job %s stayed %s, want %v", jobID, last, want)
	return nil
}

func TestAJobRunsAndItsLogIsFollowed(t *testing.T) {
	t.Parallel()

	harness := startJobs(t, func(registry *jobs.Registry) {
		err := registry.Register(jobs.Kind{Name: "example/echo", Core: true, Level: access.Use}, jobs.HandlerFunc(
			func(_ context.Context, job jobs.Job, log *jobs.Log) ([]byte, error) {
				if err := log.Write("hello from the handler"); err != nil { //nolint:contextcheck // the log carries the job's context
					return nil, err
				}
				return job.Args, nil
			}))
		if err != nil {
			t.Fatalf("registering: %v", err)
		}
	})

	job := harness.enqueue(t, &rookeryv1.JobServiceEnqueueRequest{Kind: "example/echo", ArgsCbor: []byte(`{"n":1}`)})
	done := harness.waitForState(t, job.GetId(), jobs.Succeeded)
	if string(done.GetResultCbor()) != `{"n":1}` {
		t.Errorf("the result is %q", done.GetResultCbor())
	}

	// Follow sends the line, then the end state.
	stream, err := harness.client.Follow(t.Context(), connect.NewRequest(&rookeryv1.JobServiceFollowRequest{Job: job.GetId()}))
	if err != nil {
		t.Fatalf("following: %v", err)
	}
	defer func() { _ = stream.Close() }()

	var lines []string
	var state string
	for stream.Receive() {
		message := stream.Msg()
		switch event := message.GetEvent().(type) {
		case *rookeryv1.JobServiceFollowResponse_Line:
			lines = append(lines, event.Line.GetLine())
		case *rookeryv1.JobServiceFollowResponse_Job:
			state = event.Job.GetState()
		}
	}
	if err := stream.Err(); err != nil {
		t.Fatalf("following: %v", err)
	}
	if len(lines) != 1 || lines[0] != "hello from the handler" {
		t.Errorf("the log is %q", lines)
	}
	if state != string(jobs.Succeeded) {
		t.Errorf("the end state is %q", state)
	}
}

func TestAPluginKindIsClaimedAndRetried(t *testing.T) {
	t.Parallel()

	harness := startJobs(t, func(registry *jobs.Registry) {
		err := registry.Register(jobs.Kind{Name: "example/plugin", Level: access.Use, Retries: 2, Backoff: 10 * time.Millisecond}, nil)
		if err != nil {
			t.Fatalf("registering: %v", err)
		}
	})
	worker := jobs.NewWorker(harness.store, harness.registry, nil)

	job := harness.enqueue(t, &rookeryv1.JobServiceEnqueueRequest{Kind: "example/plugin", ArgsCbor: []byte("args")})
	if job.GetMaxAttempts() != 3 {
		t.Fatalf("the plugin kind allows %d attempts, want 3", job.GetMaxAttempts())
	}

	// The worker claims the job, fails it once, and fails it again; the third
	// attempt succeeds.
	for attempt := 1; attempt <= 3; attempt++ {
		claim, err := worker.Claim(t.Context(), []string{"example/plugin"}, 10*time.Second)
		if err != nil {
			t.Fatalf("claiming: %v", err)
		}
		if claim == nil {
			t.Fatal("no job was claimed")
		}
		if claim.ID != job.GetId() {
			t.Fatalf("claimed %s, want %s", claim.ID, job.GetId())
		}
		if claim.Attempts != attempt {
			t.Fatalf("the claim says attempt %d, want %d", claim.Attempts, attempt)
		}
		if err := worker.Line(t.Context(), claim, fmt.Sprintf("attempt %d", attempt)); err != nil {
			t.Fatalf("writing a line: %v", err)
		}

		if attempt < 3 {
			if err := worker.Fail(t.Context(), claim, fmt.Errorf("not yet")); err != nil {
				t.Fatalf("failing an attempt: %v", err)
			}
			harness.waitForState(t, job.GetId(), jobs.Queued)
			continue
		}
		if err := worker.Complete(t.Context(), claim, []byte("done")); err != nil {
			t.Fatalf("completing: %v", err)
		}
	}

	done := harness.waitForState(t, job.GetId(), jobs.Succeeded)
	if done.GetAttempts() != 3 {
		t.Errorf("the job records %d attempts, want 3", done.GetAttempts())
	}
	if string(done.GetResultCbor()) != "done" {
		t.Errorf("the result is %q", done.GetResultCbor())
	}

	lines, err := harness.store.Queries().LinesAfter(t.Context(), store.LinesAfterParams{JobID: job.GetId(), Limit: 10})
	if err != nil {
		t.Fatalf("reading the log: %v", err)
	}
	if len(lines) != 3 {
		t.Fatalf("the log has %d lines, want 3", len(lines))
	}
}

func TestAFailedCoreKindHasOneAttempt(t *testing.T) {
	t.Parallel()

	harness := startJobs(t, func(registry *jobs.Registry) {
		err := registry.Register(jobs.Kind{Name: "example/broken", Core: true, Level: access.Use}, jobs.HandlerFunc(
			func(context.Context, jobs.Job, *jobs.Log) ([]byte, error) {
				return nil, fmt.Errorf("it is broken")
			}))
		if err != nil {
			t.Fatalf("registering: %v", err)
		}
	})

	job := harness.enqueue(t, &rookeryv1.JobServiceEnqueueRequest{Kind: "example/broken"})
	done := harness.waitForState(t, job.GetId(), jobs.Failed)
	if done.GetAttempts() != 1 {
		t.Errorf("a core kind ran %d times, want 1", done.GetAttempts())
	}
	if !strings.Contains(done.GetError(), "it is broken") {
		t.Errorf("the error is %q", done.GetError())
	}
}

func TestACancelledJobEndsCancelled(t *testing.T) {
	t.Parallel()

	started := make(chan struct{})
	harness := startJobs(t, func(registry *jobs.Registry) {
		err := registry.Register(jobs.Kind{Name: "example/slow", Core: true, Level: access.Use}, jobs.HandlerFunc(
			func(ctx context.Context, _ jobs.Job, _ *jobs.Log) ([]byte, error) {
				close(started)
				<-ctx.Done()
				return nil, ctx.Err()
			}))
		if err != nil {
			t.Fatalf("registering: %v", err)
		}
	})

	job := harness.enqueue(t, &rookeryv1.JobServiceEnqueueRequest{Kind: "example/slow"})
	select {
	case <-started:
	case <-time.After(10 * time.Second):
		t.Fatal("the job never started")
	}
	harness.waitForState(t, job.GetId(), jobs.Running)

	if _, err := harness.client.Cancel(t.Context(), connect.NewRequest(&rookeryv1.JobServiceCancelRequest{Job: job.GetId()})); err != nil {
		t.Fatalf("cancelling: %v", err)
	}
	harness.waitForState(t, job.GetId(), jobs.Cancelled)
}

func TestOneJobPerHostAtATime(t *testing.T) {
	t.Parallel()

	var running, most atomic.Int32
	harness := startJobs(t, func(registry *jobs.Registry) {
		err := registry.Register(jobs.Kind{Name: "example/host", Core: true, Level: access.Use, HostLock: true}, jobs.HandlerFunc(
			func(_ context.Context, _ jobs.Job, _ *jobs.Log) ([]byte, error) {
				now := running.Add(1)
				for {
					mostSeen := most.Load()
					if now <= mostSeen || most.CompareAndSwap(mostSeen, now) {
						break
					}
				}
				time.Sleep(50 * time.Millisecond)
				running.Add(-1)
				return nil, nil
			}))
		if err != nil {
			t.Fatalf("registering: %v", err)
		}
	})

	const count = 3
	ids := make([]string, 0, count)
	for range count {
		ids = append(ids, harness.enqueue(t, &rookeryv1.JobServiceEnqueueRequest{
			Kind:     "example/host",
			Resource: access.HostURI("forge"),
		}).GetId())
	}
	for _, id := range ids {
		harness.waitForState(t, id, jobs.Succeeded)
	}
	if got := most.Load(); got != 1 {
		t.Fatalf("%d jobs ran on one host at once, want 1", got)
	}
}

func TestTheLogMasksSecrets(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	ctx := t.Context()

	record, err := opened.Queries().CreateJob(ctx, store.CreateJobParams{
		ID:          "01hjob",
		Kind:        "example/echo",
		Args:        []byte{},
		MaxAttempts: 1,
	})
	if err != nil {
		t.Fatalf("making a job: %v", err)
	}

	log := jobs.NewLog(ctx, opened, record.ID, jobs.NewMasker("hunter2", "s3cret"))
	if err := log.Write("the password is hunter2 and the key is s3cret"); err != nil {
		t.Fatalf("writing a line: %v", err)
	}

	lines, err := opened.Queries().LinesAfter(ctx, store.LinesAfterParams{JobID: record.ID, Limit: 10})
	if err != nil {
		t.Fatalf("reading the log: %v", err)
	}
	if len(lines) != 1 {
		t.Fatalf("the log has %d lines, want 1", len(lines))
	}
	if strings.Contains(lines[0].Line, "hunter2") || strings.Contains(lines[0].Line, "s3cret") {
		t.Fatalf("a secret reached the log: %q", lines[0].Line)
	}
	if lines[0].Line != "the password is ******* and the key is ******" {
		t.Errorf("the line is %q", lines[0].Line)
	}
}

func TestEnqueueRefusesAnUnknownKind(t *testing.T) {
	t.Parallel()

	harness := startJobs(t, func(*jobs.Registry) {})
	if _, err := harness.client.Enqueue(t.Context(), connect.NewRequest(&rookeryv1.JobServiceEnqueueRequest{Kind: "nobody/knows"})); err == nil {
		t.Fatal("an unknown kind was enqueued")
	} else if connect.CodeOf(err) != connect.CodeInvalidArgument {
		t.Fatalf("an unknown kind failed with %v", err)
	}
}

func TestOneScheduledRunIsEnqueuedOnce(t *testing.T) {
	t.Parallel()

	var runs sync.Map
	harness := startJobs(t, func(registry *jobs.Registry) {
		err := registry.Register(jobs.Kind{Name: "example/scheduled", Core: true, Level: access.Use}, jobs.HandlerFunc(
			func(context.Context, jobs.Job, *jobs.Log) ([]byte, error) {
				runs.Store("ran", true)
				return nil, nil
			}))
		if err != nil {
			t.Fatalf("registering: %v", err)
		}
	})

	// The service's own schedule, made to be due now.
	schedules := jobs.NewSchedules(harness.store, harness.registry, harness.service, nil)
	schedule, err := schedules.Create(t.Context(), jobs.ScheduleRequest{
		Name: "nightly",
		Kind: "example/scheduled",
		Cron: "0 3 * * *",
	})
	if err != nil {
		t.Fatalf("creating a schedule: %v", err)
	}
	if schedule.NextRunAt.IsZero() {
		t.Fatal("the schedule says no next run")
	}

	made, err := schedules.RunNow(t.Context(), schedule.ID)
	if err != nil {
		t.Fatalf("running the schedule now: %v", err)
	}
	harness.waitForState(t, made.Job.ID, jobs.Succeeded)
	if _, ok := runs.Load("ran"); !ok {
		t.Fatal("the schedule's job did not run")
	}
}
