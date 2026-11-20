package server

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"time"

	"github.com/jackc/pgx/v5"
)

// leaderLock is the advisory lock's key, "rokery" in ASCII, so that nothing else
// on the database takes it by accident.
const leaderLock int64 = 0x726f6b657279

// leaderRetry is how often a standby looks at the lock again.
const leaderRetry = 2 * time.Second

// leaderPing is how often the leader checks that it still holds the lock.
const leaderPing = 5 * time.Second

// Leader is the advisory lock that makes one server of several the leader. It
// holds a connection of its own: a pooled connection could be reset, and with it
// the lock.
type Leader struct {
	conn *pgx.Conn
	log  *slog.Logger
}

// Acquire takes the leader lock, waiting as a standby for as long as it takes.
// It returns when the lock is held, or when ctx ends.
func Acquire(ctx context.Context, url string, logger *slog.Logger) (*Leader, error) {
	conn, err := pgx.Connect(ctx, url)
	if err != nil {
		return nil, fmt.Errorf("server: connecting for the leader lock: %w", err)
	}
	leader := &Leader{conn: conn, log: logger}

	standbyLogged := false
	for {
		held, err := leader.try(ctx)
		if err != nil {
			_ = conn.Close(ctx)
			return nil, err
		}
		if held {
			return leader, nil
		}
		if !standbyLogged {
			logger.Info("another server is the leader; this one is a standby")
			standbyLogged = true
		}
		select {
		case <-ctx.Done():
			_ = conn.Close(context.WithoutCancel(ctx))
			return nil, ctx.Err()
		case <-time.After(leaderRetry):
		}
	}
}

// try asks for the lock without waiting.
func (l *Leader) try(ctx context.Context) (bool, error) {
	var held bool
	if err := l.conn.QueryRow(ctx, "SELECT pg_try_advisory_lock($1)", leaderLock).Scan(&held); err != nil {
		return false, fmt.Errorf("server: asking for the leader lock: %w", err)
	}
	return held, nil
}

// Watch reports on a channel when the lock's connection is no longer good, which
// is when this server stops being the leader. The database closing the
// connection, a network that drops, and an administrator's pg_terminate_backend
// all look the same here, and all mean the same thing: exit.
func (l *Leader) Watch(ctx context.Context) <-chan error {
	lost := make(chan error, 1)
	go func() {
		defer close(lost)
		ticker := time.NewTicker(leaderPing)
		defer ticker.Stop()

		var misses int
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
			}
			pingCtx, cancel := context.WithTimeout(ctx, leaderPing)
			err := l.conn.Ping(pingCtx)
			cancel()
			if err == nil {
				misses = 0
				continue
			}
			if ctx.Err() != nil {
				return
			}
			misses++
			if misses < 2 {
				continue
			}
			l.log.Error("lost the leader lock's connection", slog.String("error", err.Error()))
			lost <- fmt.Errorf("server: the leader lock's connection is gone: %w", err)
			return
		}
	}()
	return lost
}

// Close closes the lock's connection, which releases the lock.
func (l *Leader) Close(ctx context.Context) {
	if l == nil || l.conn == nil {
		return
	}
	if _, err := l.conn.Exec(ctx, "SELECT pg_advisory_unlock($1)", leaderLock); err != nil && !errors.Is(err, context.Canceled) {
		l.log.Debug("releasing the leader lock", slog.String("error", err.Error()))
	}
	if err := l.conn.Close(ctx); err != nil {
		l.log.Debug("closing the leader lock's connection", slog.String("error", err.Error()))
	}
}
