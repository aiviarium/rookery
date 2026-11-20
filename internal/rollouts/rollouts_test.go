package rollouts_test

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/messaging"
	"github.com/aiviarium/rookery/internal/rollouts"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// harness is a test's roll-outs: a runner working the child jobs, the roll-out
// service in front, and the advance loop driven by hand.
type harness struct {
	store         *store.Store
	jobs          *jobs.Service
	service       *rollouts.Service
	manager       *rollouts.Manager
	notifications *messaging.Notifications
	// outcomes says how a host's apply ends: nil succeeds, an error fails it.
	outcomes func(host string) error
}

// startRollouts runs a job runner with host.apply and host.action, and a
// roll-out service over it. The caller's principal is an admin.
func startRollouts(t *testing.T, hosts rollouts.HostSelector) *harness {
	t.Helper()

	opened := storetest.New(t)
	checker := access.NewChecker(opened)
	registry := jobs.NewRegistry()

	harness := &harness{store: opened, notifications: messaging.NewNotifications(opened)}
	apply := jobs.HandlerFunc(func(_ context.Context, job jobs.Job, log *jobs.Log) ([]byte, error) {
		host := job.Resource
		if harness.outcomes != nil {
			if err := harness.outcomes(host); err != nil {
				return nil, err
			}
		}
		if err := log.Write("worked " + host); err != nil { //nolint:contextcheck // the log carries the job's context
			return nil, err
		}
		return []byte(`{"ok":true}`), nil
	})
	for _, kind := range []string{rollouts.KindApply, rollouts.KindAction} {
		if err := registry.Register(jobs.Kind{Name: kind, Core: true, Level: access.Manage, HostLock: true}, apply); err != nil {
			t.Fatalf("registering %s: %v", kind, err)
		}
	}

	runner := jobs.NewRunner(opened, registry, nil, nil)
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

	harness.jobs = jobs.NewService(opened, checker, registry, runner, nil)
	harness.service = rollouts.New(rollouts.Options{
		Store:         opened,
		Checker:       checker,
		Jobs:          harness.jobs,
		Hosts:         hosts,
		Notifications: harness.notifications,
	})
	harness.manager = rollouts.NewManager(harness.service)
	return harness
}

// userContext is a context a user with the admin flag calls in.
func userContext(t *testing.T) context.Context {
	t.Helper()
	return access.WithPrincipal(t.Context(), access.Principal{Kind: access.KindUser, ID: "01huser", Admin: true})
}

// asAdmin is a context the local admin calls in.
func asAdmin(t *testing.T) context.Context {
	t.Helper()
	return access.WithPrincipal(t.Context(), access.LocalAdmin())
}

// started is a roll-out started by the test, with the failure fatal.
func (h *harness) started(t *testing.T, request rollouts.StartRequest) rollouts.Rollout {
	t.Helper()

	rollout, err := h.service.Start(asAdmin(t), request)
	if err != nil {
		t.Fatalf("starting the roll-out: %v", err)
	}
	return rollout
}

// waitFor advances the roll-outs until one is in a state.
func (h *harness) waitFor(t *testing.T, rolloutID string, want ...rollouts.State) rollouts.Rollout {
	t.Helper()

	deadline := time.Now().Add(20 * time.Second)
	var last rollouts.State
	for time.Now().Before(deadline) {
		if err := h.manager.Advance(t.Context()); err != nil {
			t.Fatalf("advancing: %v", err)
		}
		rollout, err := h.service.Get(t.Context(), rolloutID)
		if err != nil {
			t.Fatalf("reading the roll-out: %v", err)
		}
		last = rollout.State
		for _, state := range want {
			if last == state {
				return rollout
			}
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("the roll-out stayed %s, want %v", last, want)
	return rollouts.Rollout{}
}

// fakeHosts is a host registry for a test.
type fakeHosts struct {
	hosts []rollouts.Candidate
	err   error
}

// Hosts returns the hosts of the fake registry.
func (f fakeHosts) Hosts(context.Context) ([]rollouts.Candidate, error) {
	return f.hosts, f.err
}

func TestParseSelector(t *testing.T) {
	t.Parallel()

	labels := map[string]string{"role": "desk", "site": "lab"}

	for _, test := range []struct {
		expression string
		want       bool
	}{
		{"role=desk", true},
		{"role=desk,site=lab", true},
		{"role=desk,site=home", false},
		{"role!=server", true},
		{"role!=desk", false},
		{"role", true},
		{"gpu", false},
		{"site!=lab,role=desk", false},
		{"", true},
	} {
		selector, err := rollouts.ParseSelector(test.expression)
		if err != nil {
			t.Fatalf("parsing %q: %v", test.expression, err)
		}
		if got := selector.Match(labels); got != test.want {
			t.Errorf("%q matched %v, want %v", test.expression, got, test.want)
		}
	}

	for _, bad := range []string{"=desk", "role=desk,,site=lab", "ro le=desk"} {
		if _, err := rollouts.ParseSelector(bad); err == nil {
			t.Errorf("%q parsed, and should not have", bad)
		}
	}
}

func TestPlanSelectsAndSkipsHosts(t *testing.T) {
	t.Parallel()

	hosts := fakeHosts{hosts: []rollouts.Candidate{
		{Name: "alpha", Labels: map[string]string{"role": "desk"}, Current: false},
		{Name: "bravo", Labels: map[string]string{"role": "desk"}, Current: true},
		{Name: "charlie", Labels: map[string]string{"role": "server"}, Current: false},
		{Name: "delta", Labels: map[string]string{"role": "desk"}, Skip: "the host has no desktop"},
	}}
	harness := startRollouts(t, hosts)

	// By label: the desks, and the one that cannot take it is skipped with the
	// reason.
	plan, err := harness.service.Plan(t.Context(), rollouts.KindApply, rollouts.Selection{Labels: "role=desk"})
	if err != nil {
		t.Fatalf("planning: %v", err)
	}
	if names(plan.Hosts) != "alpha,bravo" {
		t.Errorf("the plan works %v", names(plan.Hosts))
	}
	if len(plan.Skipped) != 1 || plan.Skipped[0].Name != "delta" || plan.Skipped[0].Reason == "" {
		t.Errorf("the plan skips %+v", plan.Skipped)
	}

	// By label and behind: bravo is current, so it is not in.
	plan, err = harness.service.Plan(t.Context(), rollouts.KindApply, rollouts.Selection{Labels: "role=desk", Behind: true})
	if err != nil {
		t.Fatalf("planning: %v", err)
	}
	if names(plan.Hosts) != "alpha" {
		t.Errorf("the plan works %v", names(plan.Hosts))
	}

	// The registry excludes a name that was never recorded.
	plan, err = harness.service.Plan(t.Context(), rollouts.KindApply, rollouts.Selection{Hosts: []string{"zulu"}})
	if err != nil {
		t.Fatalf("planning: %v", err)
	}
	if len(plan.Hosts) != 0 {
		t.Errorf("the plan works %v", names(plan.Hosts))
	}
}

func TestPlanRefusesWhatOnlyARegistryCanAnswer(t *testing.T) {
	t.Parallel()

	harness := startRollouts(t, nil)
	if _, err := harness.service.Plan(t.Context(), rollouts.KindApply, rollouts.Selection{Labels: "role=desk"}); !errors.Is(err, access.ErrUnavailable) {
		t.Fatalf("planning by label without a registry gave %v", err)
	}
	if _, err := harness.service.Plan(t.Context(), rollouts.KindApply, rollouts.Selection{}); !errors.Is(err, access.ErrInvalid) {
		t.Fatalf("planning an empty selection gave %v", err)
	}
	if _, err := harness.service.Plan(t.Context(), "example/other", rollouts.Selection{Hosts: []string{"alpha"}}); !errors.Is(err, access.ErrInvalid) {
		t.Fatalf("planning another kind gave %v", err)
	}
}

func TestRolloutRunsItsHostsInBatches(t *testing.T) {
	t.Parallel()

	harness := startRollouts(t, nil)
	rollout := harness.started(t, rollouts.StartRequest{
		Kind:      rollouts.KindApply,
		Resource:  "space://01hspace",
		Selection: rollouts.Selection{Hosts: []string{"alpha", "bravo", "charlie"}},
		Strategy:  rollouts.Strategy{BatchSize: 2},
	})
	if rollout.State != rollouts.Running {
		t.Fatalf("the roll-out is %s", rollout.State)
	}
	if len(rollout.Hosts) != 3 {
		t.Fatalf("the roll-out has %d hosts", len(rollout.Hosts))
	}

	done := harness.waitFor(t, rollout.ID, rollouts.Succeeded)
	if counts := done.Counts(); counts.Succeeded != 3 {
		t.Errorf("the roll-out ended with %+v", counts)
	}

	// Each host has a child job whose parent is the roll-out.
	children, err := harness.jobs.List(t.Context(), jobs.Filter{Resource: "host://alpha"})
	if err != nil {
		t.Fatalf("listing the child jobs: %v", err)
	}
	if len(children) != 1 || children[0].Parent != rollout.ID || children[0].Kind != rollouts.KindApply {
		t.Fatalf("the host's jobs are %+v", children)
	}
}

func TestRolloutPausesWhenAHostFails(t *testing.T) {
	t.Parallel()

	harness := startRollouts(t, nil)
	if _, err := harness.store.Queries().CreateUser(t.Context(), store.CreateUserParams{
		ID: "01huser", Email: "user@example.test", Name: "user", Admin: true,
	}); err != nil {
		t.Fatalf("making the user: %v", err)
	}
	harness.outcomes = func(host string) error {
		if host == "host://bravo" {
			return errors.New("the host said no")
		}
		return nil
	}
	rollout, err := harness.service.Start(userContext(t), rollouts.StartRequest{
		Kind:      rollouts.KindApply,
		Selection: rollouts.Selection{Hosts: []string{"alpha", "bravo", "charlie"}},
		Strategy:  rollouts.Strategy{BatchSize: 3},
	})
	if err != nil {
		t.Fatalf("starting the roll-out: %v", err)
	}

	paused := harness.waitFor(t, rollout.ID, rollouts.Paused)
	if paused.PausedBecause == "" {
		t.Error("the roll-out paused without saying why")
	}

	// The creator is told, once.
	notes, err := harness.notifications.List(t.Context(), "01huser", "", false, 10)
	if err != nil {
		t.Fatalf("listing the notifications: %v", err)
	}
	if len(notes) != 1 || notes[0].Resource != access.RolloutURI(rollout.ID) {
		t.Fatalf("the notifications are %+v", notes)
	}

	// A resume carries on, and the failed host is not tried again.
	resumed, err := harness.service.Resume(t.Context(), rollout.ID)
	if err != nil {
		t.Fatalf("resuming: %v", err)
	}
	if resumed.State != rollouts.Running {
		t.Fatalf("the resumed roll-out is %s", resumed.State)
	}
	finished := harness.waitFor(t, rollout.ID, rollouts.Failed)
	if got := finished.Counts(); got.Failed != 1 || got.Succeeded != 2 {
		t.Fatalf("the resumed roll-out ended with %+v", got)
	}
}

func TestCanaryWaitsForResume(t *testing.T) {
	t.Parallel()

	harness := startRollouts(t, nil)
	rollout := harness.started(t, rollouts.StartRequest{
		Kind:      rollouts.KindApply,
		Selection: rollouts.Selection{Hosts: []string{"alpha", "bravo", "charlie"}},
		Strategy:  rollouts.Strategy{Canary: 1, BatchSize: 2},
	})

	paused := harness.waitFor(t, rollout.ID, rollouts.Paused)
	counts := paused.Counts()
	if counts.Succeeded != 1 || counts.Pending != 2 {
		t.Fatalf("the canary left %+v", counts)
	}

	if _, err := harness.service.Resume(t.Context(), rollout.ID); err != nil {
		t.Fatalf("resuming: %v", err)
	}
	done := harness.waitFor(t, rollout.ID, rollouts.Succeeded)
	if done.Counts().Succeeded != 3 {
		t.Errorf("the roll-out ended with %+v", done.Counts())
	}
}

func TestCancelStopsTheRunningJobs(t *testing.T) {
	t.Parallel()

	harness := startRollouts(t, nil)
	release := make(chan struct{})
	harness.outcomes = func(string) error {
		<-release
		return nil
	}
	rollout := harness.started(t, rollouts.StartRequest{
		Kind:      rollouts.KindApply,
		Selection: rollouts.Selection{Hosts: []string{"alpha", "bravo"}},
		Strategy:  rollouts.Strategy{BatchSize: 1},
	})

	// Wait until the first host's job runs, then cancel.
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		job, err := harness.jobs.List(t.Context(), jobs.Filter{Resource: "host://alpha"})
		if err != nil {
			t.Fatalf("listing the host's jobs: %v", err)
		}
		if len(job) == 1 && job[0].State == jobs.Running {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	cancelled, err := harness.service.Cancel(t.Context(), rollout.ID)
	if err != nil {
		t.Fatalf("cancelling: %v", err)
	}
	close(release)
	if cancelled.State != rollouts.Cancelled {
		t.Fatalf("the cancelled roll-out is %s", cancelled.State)
	}
	counts := cancelled.Counts()
	if counts.Cancelled != 2 || counts.Succeeded != 0 {
		t.Fatalf("the cancelled roll-out has %+v", counts)
	}
}

func TestWatchStreamsUntilTheRolloutEnds(t *testing.T) {
	t.Parallel()

	harness := startRollouts(t, nil)
	rollout := harness.started(t, rollouts.StartRequest{
		Kind:      rollouts.KindApply,
		Selection: rollouts.Selection{Hosts: []string{"alpha"}},
	})

	var mu sync.Mutex
	var seen []rollouts.State
	done := make(chan error, 1)
	go func() {
		done <- harness.service.Watch(t.Context(), rollout.ID, func(found rollouts.Rollout) error {
			mu.Lock()
			defer mu.Unlock()
			seen = append(seen, found.State)
			return nil
		})
	}()

	// The advance loop runs while the watch streams, as it does on a server.
	deadline := time.After(20 * time.Second)
	for ended := false; !ended; {
		select {
		case err := <-done:
			if err != nil {
				t.Fatalf("watching: %v", err)
			}
			ended = true
		case <-deadline:
			t.Fatal("the watch did not end")
		case <-time.After(20 * time.Millisecond):
			if err := harness.manager.Advance(t.Context()); err != nil {
				t.Fatalf("advancing: %v", err)
			}
		}
	}

	mu.Lock()
	defer mu.Unlock()
	if len(seen) == 0 || seen[len(seen)-1] != rollouts.Succeeded {
		t.Fatalf("the watch saw %v", seen)
	}
}

func TestPauseBetweenBatches(t *testing.T) {
	t.Parallel()

	harness := startRollouts(t, nil)
	rollout := harness.started(t, rollouts.StartRequest{
		Kind:      rollouts.KindApply,
		Selection: rollouts.Selection{Hosts: []string{"alpha", "bravo"}},
		Strategy:  rollouts.Strategy{BatchSize: 1, Pause: 30 * time.Millisecond},
	})

	// The first batch runs; then the roll-out waits, still running.
	deadline := time.Now().Add(20 * time.Second)
	var waited bool
	for time.Now().Before(deadline) && !waited {
		if err := harness.manager.Advance(t.Context()); err != nil {
			t.Fatalf("advancing: %v", err)
		}
		found, err := harness.service.Get(t.Context(), rollout.ID)
		if err != nil {
			t.Fatalf("reading the roll-out: %v", err)
		}
		if !found.PausedUntil.IsZero() && found.Counts().Pending > 0 {
			waited = true
		}
		time.Sleep(10 * time.Millisecond)
	}
	if !waited {
		t.Fatal("the roll-out never waited between its batches")
	}

	done := harness.waitFor(t, rollout.ID, rollouts.Succeeded)
	if done.Counts().Succeeded != 2 {
		t.Errorf("the roll-out ended with %+v", done.Counts())
	}
}

func TestLeftRunningRolloutsArePaused(t *testing.T) {
	t.Parallel()

	harness := startRollouts(t, nil)
	release := make(chan struct{})
	harness.outcomes = func(string) error {
		<-release
		return nil
	}
	rollout := harness.started(t, rollouts.StartRequest{
		Kind:      rollouts.KindApply,
		Selection: rollouts.Selection{Hosts: []string{"alpha"}},
	})
	if rollout.State != rollouts.Running {
		t.Fatalf("the roll-out is %s", rollout.State)
	}
	close(release)

	paused, err := rollouts.PauseLeftRunning(t.Context(), harness.store)
	if err != nil {
		t.Fatalf("pausing the roll-outs left running: %v", err)
	}
	if paused != 1 {
		t.Fatalf("paused %d roll-outs", paused)
	}
	found, err := harness.service.Get(t.Context(), rollout.ID)
	if err != nil {
		t.Fatalf("reading the roll-out: %v", err)
	}
	if found.State != rollouts.Paused {
		t.Fatalf("the roll-out is %s", found.State)
	}
}

// names is the names of a plan's hosts, comma-separated.
func names(hosts []rollouts.Host) string {
	out := ""
	for i, host := range hosts {
		if i > 0 {
			out += ","
		}
		out += host.Name
	}
	return out
}

func TestAssignsBatchesOfAPercentage(t *testing.T) {
	t.Parallel()

	harness := startRollouts(t, nil)
	rollout := harness.started(t, rollouts.StartRequest{
		Kind:      rollouts.KindApply,
		Selection: rollouts.Selection{Hosts: []string{"alpha", "bravo", "charlie", "delta"}},
		Strategy:  rollouts.Strategy{BatchPercent: 50},
	})
	batches := map[string]int{}
	for _, host := range rollout.Hosts {
		batches[host.Name] = host.Batch
	}
	want := map[string]int{"alpha": 0, "bravo": 0, "charlie": 1, "delta": 1}
	for host, batch := range want {
		if batches[host] != batch {
			t.Errorf("%s is in batch %d, want %d", host, batches[host], batch)
		}
	}
}
