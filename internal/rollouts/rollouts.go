// Package rollouts is an apply, or one action, on a set of hosts in batches.
//
// A roll-out is a record with a state, a row per host, and a child job per host
// that does the work. It plans its hosts from names, a label selector or the
// hosts that are behind, runs them in batches, and pauses when hosts fail or
// when its canary has run. After a leader change a running roll-out is paused,
// never resumed by itself.
package rollouts

import (
	"fmt"
	"math"
	"time"

	"google.golang.org/protobuf/types/known/durationpb"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/store"
)

// State is where a roll-out is.
type State string

// The states a roll-out is in.
const (
	// Running is a roll-out that starts its batches as they finish.
	Running State = "running"
	// Paused is a roll-out that starts no more until it is resumed.
	Paused State = "paused"
	// Succeeded is a roll-out every host of which succeeded.
	Succeeded State = "succeeded"
	// Failed is a roll-out that ended with at least one host failed.
	Failed State = "failed"
	// Cancelled is a roll-out a caller stopped.
	Cancelled State = "cancelled"
)

// Done reports whether a roll-out has ended.
func (s State) Done() bool {
	switch s {
	case Succeeded, Failed, Cancelled:
		return true
	default:
		return false
	}
}

// HostState is what a roll-out has done to one host.
type HostState string

// The states of a host of a roll-out.
const (
	// HostPending is a host whose turn has not come.
	HostPending HostState = "pending"
	// HostRunning is a host with a job working it.
	HostRunning HostState = "running"
	// HostSucceeded is a host whose job succeeded.
	HostSucceeded HostState = "succeeded"
	// HostFailed is a host whose job failed.
	HostFailed HostState = "failed"
	// HostSkipped is a host the selection named that cannot take the work.
	HostSkipped HostState = "skipped"
	// HostCancelled is a host the roll-out never reached.
	HostCancelled HostState = "cancelled"
)

// Kinds are the child job kinds a roll-out runs: an apply, or one action.
const (
	KindApply  = "host.apply"
	KindAction = "host.action"
)

// Selection is which hosts a roll-out is for. The criteria combine: a host is in
// when it meets every one that is set.
type Selection struct {
	// Hosts are hosts by name.
	Hosts []string `cbor:"hosts,omitempty"`
	// Labels is a label selector, such as "role=desk,site!=lab".
	Labels string `cbor:"labels,omitempty"`
	// Behind takes the hosts whose applied digest is not what their profile
	// asks for.
	Behind bool `cbor:"behind,omitempty"`
}

// Empty reports whether the selection names nothing at all.
func (s Selection) Empty() bool {
	return len(s.Hosts) == 0 && s.Labels == "" && !s.Behind
}

// Validate checks a selection.
func (s Selection) Validate() error {
	if s.Empty() {
		return fmt.Errorf("%w: a roll-out names hosts, a label selector or the hosts that are behind", access.ErrInvalid)
	}
	for _, host := range s.Hosts {
		if host == "" {
			return fmt.Errorf("%w: a host with no name", access.ErrInvalid)
		}
	}
	if s.Labels != "" {
		if _, err := ParseSelector(s.Labels); err != nil {
			return err
		}
	}
	return nil
}

// Strategy is how a roll-out runs.
type Strategy struct {
	// BatchSize is how many hosts run at once. Zero takes BatchPercent.
	BatchSize int `cbor:"batch_size,omitempty"`
	// BatchPercent is a batch's size as a percentage of the selected hosts.
	BatchPercent int `cbor:"batch_percent,omitempty"`
	// FailuresAllowed is how many hosts may fail before the roll-out pauses.
	// One when zero.
	FailuresAllowed int `cbor:"failures_allowed,omitempty"`
	// Pause is how long to wait after a batch before the next one starts.
	Pause time.Duration `cbor:"pause,omitempty"`
	// Canary is how many hosts run first, as a batch of their own, before the
	// roll-out waits for Resume. Zero has no canary.
	Canary int `cbor:"canary,omitempty"`
}

// Normalize fills in the defaults, so that two strategies that say the same
// thing have one encoding.
func (s Strategy) Normalize() Strategy {
	if s.FailuresAllowed <= 0 {
		s.FailuresAllowed = 1
	}
	if s.BatchSize <= 0 && s.BatchPercent <= 0 {
		// One host at a time: slow, and the least surprising default.
		s.BatchSize = 1
	}
	if s.BatchSize < 0 {
		s.BatchSize = 0
	}
	if s.BatchPercent < 0 {
		s.BatchPercent = 0
	}
	if s.Pause < 0 {
		s.Pause = 0
	}
	if s.Canary < 0 {
		s.Canary = 0
	}
	return s
}

// Validate checks a strategy.
func (s Strategy) Validate() error {
	if s.BatchPercent > 100 {
		return fmt.Errorf("%w: a batch of %d%% is more than all of the hosts", access.ErrInvalid, s.BatchPercent)
	}
	if s.BatchSize < 0 || s.BatchPercent < 0 || s.Canary < 0 || s.FailuresAllowed < 0 || s.Pause < 0 {
		return fmt.Errorf("%w: a strategy has no negative numbers", access.ErrInvalid)
	}
	return nil
}

// batchSize is how many hosts a batch holds out of total.
func (s Strategy) batchSize(total int) int {
	if s.BatchSize > 0 {
		return s.BatchSize
	}
	if s.BatchPercent > 0 {
		size := int(math.Ceil(float64(total) * float64(s.BatchPercent) / 100))
		if size < 1 {
			size = 1
		}
		return size
	}
	return 1
}

// Host is one host of a roll-out, and what became of it.
type Host struct {
	Name       string
	State      HostState
	Batch      int
	Job        string
	Reason     string
	Err        string
	StartedAt  time.Time
	FinishedAt time.Time
}

// Message is a host as the edge API has it.
func (h Host) Message() *rookeryv1.RolloutHost {
	out := &rookeryv1.RolloutHost{
		Host:   h.Name,
		State:  string(h.State),
		Batch:  int32(min(h.Batch, math.MaxInt32)), //nolint:gosec // clamped to int32
		Job:    h.Job,
		Reason: h.Reason,
		Error:  h.Err,
	}
	if !h.StartedAt.IsZero() {
		out.StartedAt = timestamppb.New(h.StartedAt)
	}
	if !h.FinishedAt.IsZero() {
		out.FinishedAt = timestamppb.New(h.FinishedAt)
	}
	return out
}

// Rollout is an apply or an action on a set of hosts, in batches.
type Rollout struct {
	ID            string
	Kind          string
	Args          []byte
	Resource      string
	CreatedBy     string
	State         State
	Selection     Selection
	Strategy      Strategy
	CanaryDone    bool
	failuresSeen  int
	PausedUntil   time.Time
	PausedBecause string
	CreatedAt     time.Time
	StartedAt     time.Time
	FinishedAt    time.Time
	// Hosts are the hosts the roll-out works, and Skipped the ones the
	// selection named that cannot take it.
	Hosts   []Host
	Skipped []Host
}

// Counts is how many hosts of a roll-out are in each state.
type Counts struct {
	Pending   int
	Running   int
	Succeeded int
	Failed    int
	Cancelled int
}

// Counts counts the hosts of a roll-out.
func (r Rollout) Counts() Counts {
	var counts Counts
	for _, host := range r.Hosts {
		switch host.State {
		case HostPending:
			counts.Pending++
		case HostRunning:
			counts.Running++
		case HostSucceeded:
			counts.Succeeded++
		case HostFailed:
			counts.Failed++
		case HostCancelled:
			counts.Cancelled++
		}
	}
	return counts
}

// Message is a roll-out as the edge API has it.
func (r Rollout) Message() *rookeryv1.Rollout {
	counts := r.Counts()
	out := &rookeryv1.Rollout{
		Id:            r.ID,
		Kind:          r.Kind,
		ArgsCbor:      r.Args,
		Resource:      r.Resource,
		CreatedBy:     r.CreatedBy,
		State:         string(r.State),
		Selection:     r.Selection.Message(),
		Strategy:      r.Strategy.Message(),
		Hosts:         make([]*rookeryv1.RolloutHost, 0, len(r.Hosts)),
		Skipped:       make([]*rookeryv1.RolloutHost, 0, len(r.Skipped)),
		Pending:       int32(min(counts.Pending, math.MaxInt32)),   //nolint:gosec // clamped to int32
		Running:       int32(min(counts.Running, math.MaxInt32)),   //nolint:gosec // clamped to int32
		Succeeded:     int32(min(counts.Succeeded, math.MaxInt32)), //nolint:gosec // clamped to int32
		Failed:        int32(min(counts.Failed, math.MaxInt32)),    //nolint:gosec // clamped to int32
		PausedBecause: r.PausedBecause,
	}
	for _, host := range r.Hosts {
		out.Hosts = append(out.Hosts, host.Message())
	}
	for _, host := range r.Skipped {
		out.Skipped = append(out.Skipped, host.Message())
	}
	if !r.PausedUntil.IsZero() {
		out.PausedUntil = timestamppb.New(r.PausedUntil)
	}
	if !r.CreatedAt.IsZero() {
		out.CreatedAt = timestamppb.New(r.CreatedAt)
	}
	if !r.StartedAt.IsZero() {
		out.StartedAt = timestamppb.New(r.StartedAt)
	}
	if !r.FinishedAt.IsZero() {
		out.FinishedAt = timestamppb.New(r.FinishedAt)
	}
	return out
}

// Message is a selection as the edge API has it.
func (s Selection) Message() *rookeryv1.RolloutSelection {
	return &rookeryv1.RolloutSelection{Hosts: s.Hosts, Labels: s.Labels, Behind: s.Behind}
}

// Message is a strategy as the edge API has it.
func (s Strategy) Message() *rookeryv1.RolloutStrategy {
	out := &rookeryv1.RolloutStrategy{
		BatchSize:       int32(min(s.BatchSize, math.MaxInt32)),       //nolint:gosec // clamped to int32
		BatchPercent:    int32(min(s.BatchPercent, math.MaxInt32)),    //nolint:gosec // clamped to int32
		FailuresAllowed: int32(min(s.FailuresAllowed, math.MaxInt32)), //nolint:gosec // clamped to int32
		Canary:          int32(min(s.Canary, math.MaxInt32)),          //nolint:gosec // clamped to int32
	}
	if s.Pause > 0 {
		out.Pause = durationpb.New(s.Pause)
	}
	return out
}

// selectionOf is a selection as the edge API has it, and back.
func selectionOf(message *rookeryv1.RolloutSelection) Selection {
	if message == nil {
		return Selection{}
	}
	return Selection{Hosts: message.GetHosts(), Labels: message.GetLabels(), Behind: message.GetBehind()}
}

// strategyOf is a strategy as the edge API has it.
func strategyOf(message *rookeryv1.RolloutStrategy) Strategy {
	if message == nil {
		return Strategy{}
	}
	return Strategy{
		BatchSize:       int(message.GetBatchSize()),
		BatchPercent:    int(message.GetBatchPercent()),
		FailuresAllowed: int(message.GetFailuresAllowed()),
		Pause:           message.GetPause().AsDuration(),
		Canary:          int(message.GetCanary()),
	}
}

// rolloutOf is a roll-out from the database.
func rolloutOf(record store.RookeryRollouts) (Rollout, error) {
	rollout := Rollout{
		ID:            record.ID,
		Kind:          record.Kind,
		Args:          record.Args,
		Resource:      record.Resource,
		CreatedBy:     record.CreatedBy,
		State:         State(record.State),
		CanaryDone:    record.CanaryDone,
		failuresSeen:  int(record.FailuresSeen),
		PausedBecause: record.PausedBecause,
	}
	if len(record.Selection) > 0 {
		if err := decb(record.Selection, &rollout.Selection); err != nil {
			return Rollout{}, err
		}
	}
	if len(record.Strategy) > 0 {
		if err := decb(record.Strategy, &rollout.Strategy); err != nil {
			return Rollout{}, err
		}
	}
	if !record.CreatedAt.IsZero() {
		rollout.CreatedAt = record.CreatedAt
	}
	if record.PausedUntil != nil {
		rollout.PausedUntil = *record.PausedUntil
	}
	if record.StartedAt != nil {
		rollout.StartedAt = *record.StartedAt
	}
	if record.FinishedAt != nil {
		rollout.FinishedAt = *record.FinishedAt
	}
	return rollout, nil
}

// hostOf is a host from the database.
func hostOf(record store.RookeryRolloutHosts) Host {
	host := Host{
		Name:   record.Host,
		State:  HostState(record.State),
		Batch:  int(record.Batch),
		Job:    record.Job,
		Reason: record.Reason,
		Err:    record.Error,
	}
	if record.StartedAt != nil {
		host.StartedAt = *record.StartedAt
	}
	if record.FinishedAt != nil {
		host.FinishedAt = *record.FinishedAt
	}
	return host
}

// decb reads a value a row holds as CBOR.
func decb(data []byte, value any) error {
	if err := cborx.Unmarshal(data, value); err != nil {
		return fmt.Errorf("rollouts: reading a stored value: %w", err)
	}
	return nil
}
