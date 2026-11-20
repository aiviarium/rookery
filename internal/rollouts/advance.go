package rollouts

import (
	"context"
	"fmt"
	"log/slog"
	"time"

	"github.com/aiviarium/rookery/internal/store"
)

// DefaultInterval is how often the advance loop looks for roll-outs that can
// move.
const DefaultInterval = 2 * time.Second

// Manager moves roll-outs on. It is one loop on the leader: it reads how the
// hosts of each running roll-out ended, starts the next batch, and pauses or
// ends the roll-out.
type Manager struct {
	service  *Service
	interval time.Duration
}

// NewManager returns the advance loop of a roll-out service.
func NewManager(service *Service) *Manager {
	return &Manager{service: service, interval: DefaultInterval}
}

// Run advances the roll-outs until ctx ends.
func (m *Manager) Run(ctx context.Context) error {
	ticker := time.NewTicker(m.interval)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
			if err := m.Advance(ctx); err != nil && ctx.Err() == nil {
				m.service.logger.Error("advancing the roll-outs", slog.String("error", err.Error()))
			}
		}
	}
}

// Advance is one pass over the roll-outs that can move.
func (m *Manager) Advance(ctx context.Context) error {
	running, err := m.service.store.Queries().ListRunnableRollouts(ctx, 100)
	if err != nil {
		return fmt.Errorf("rollouts: reading the roll-outs that can move: %w", err)
	}
	for _, record := range running {
		if err := m.service.advanceOne(ctx, record.ID); err != nil && ctx.Err() == nil {
			m.service.logger.Error("advancing a roll-out",
				slog.String("rollout", record.ID), slog.String("error", err.Error()))
		}
	}
	return nil
}

// PauseLeftRunning pauses the roll-outs a stopped server left running, and
// returns how many it paused. Start-up calls it before the advance loop: after a
// leader change a roll-out is never resumed by itself.
func PauseLeftRunning(ctx context.Context, opened *store.Store) (int64, error) {
	paused, err := opened.Queries().PauseRunningRollouts(ctx)
	if err != nil {
		return 0, fmt.Errorf("rollouts: pausing the roll-outs a stopped server left running: %w", err)
	}
	return paused, nil
}
