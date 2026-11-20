package hosts

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"math"
	"strings"
	"sync"
	"time"

	"connectrpc.com/connect"
	"github.com/jackc/pgx/v5"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/store"
)

const hostFollowInterval = 2 * time.Second

// FollowChannels keeps one follower per managed host until the leader stops.
func (r *Runtime) FollowChannels(ctx context.Context) error {
	ticker := time.NewTicker(hostFollowInterval)
	defer ticker.Stop()
	followers := make(map[string]context.CancelFunc)
	var wg sync.WaitGroup
	defer func() {
		for _, cancel := range followers {
			cancel()
		}
		wg.Wait()
	}()

	log := r.hosts.deps.Log
	if log == nil {
		log = slog.Default()
	}
	for {
		hosts, err := r.hosts.deps.Hosts.List(ctx, Filter{})
		if err != nil && ctx.Err() == nil {
			log.WarnContext(ctx, "listing hosts to follow", "error", err)
		}
		if err == nil {
			wanted := make(map[string]bool, len(hosts))
			for _, host := range hosts {
				if !Can(host.Type, CapabilityChannel) {
					continue
				}
				wanted[host.Name] = true
				if _, following := followers[host.Name]; following {
					continue
				}
				followerCtx, cancel := context.WithCancel(ctx)
				followers[host.Name] = cancel
				wg.Go(func() { r.followChannel(followerCtx, host.Name, log) })
			}
			for name, cancel := range followers {
				if !wanted[name] {
					cancel()
					delete(followers, name)
					r.ForgetTerminals(name)
				}
			}
		}

		select {
		case <-ctx.Done():
			return nil
		case <-ticker.C:
		}
	}
}

func (r *Runtime) followChannel(ctx context.Context, host string, log *slog.Logger) {
	for ctx.Err() == nil {
		if err := r.readChannel(ctx, host); err != nil && ctx.Err() == nil {
			log.DebugContext(ctx, "host channel disconnected", "host", host, "error", err)
		}
		select {
		case <-ctx.Done():
			return
		case <-time.After(hostFollowInterval):
		}
	}
}

func (r *Runtime) readChannel(ctx context.Context, host string) error {
	opened := r.hosts.deps.Hosts.store
	cursor, err := opened.Queries().GetHostCursor(ctx, host)
	if err != nil && !errors.Is(err, pgx.ErrNoRows) {
		return fmt.Errorf("hosts: reading %s channel cursor: %w", host, err)
	}
	client, release, err := r.hosts.deps.Links.Use(ctx, host)
	if err != nil {
		return err
	}
	defer release()
	if _, err := r.syncTerminals(ctx, host, client); err != nil {
		return err
	}
	if err := r.hosts.deps.Hosts.Touch(ctx, host); err != nil {
		return err
	}
	follow, err := client.Channel.Follow(ctx, connect.NewRequest(&hostv1.ChannelServiceFollowRequest{
		Since: uint64(max(cursor.Sequence, 0)), //nolint:gosec // clamped to zero
	}))
	if err != nil {
		return err
	}
	defer func() { _ = follow.Close() }()
	for follow.Receive() {
		if err := takeChannelMessage(ctx, opened, host, follow.Msg().GetMessage()); err != nil {
			return err
		}
	}
	return follow.Err()
}

// The cursor and message commit together. Replaying a delivery after a
// dropped link or interrupted transaction cannot publish a duplicate.
func takeChannelMessage(ctx context.Context, opened *store.Store, host string, message *hostv1.ChannelMessage) error {
	sequence := message.GetSequence()
	topic := strings.TrimSpace(message.GetTopic())
	if sequence == 0 || sequence > math.MaxInt64 || topic == "" {
		return fmt.Errorf("hosts: invalid channel message from %s", host)
	}

	tx, err := opened.Pool().Begin(ctx)
	if err != nil {
		return fmt.Errorf("hosts: beginning a channel publish: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()
	queries := opened.Queries().WithTx(tx)
	advanced, err := queries.AdvanceHostCursor(ctx, store.AdvanceHostCursorParams{
		Host: host, Sequence: int64(sequence), //nolint:gosec // bounded above
	})
	if err != nil {
		return fmt.Errorf("hosts: advancing %s channel cursor: %w", host, err)
	}
	if advanced == 0 {
		return nil
	}
	topic = "host/" + host + "/" + topic
	if err := queries.EnsureTopic(ctx, topic); err != nil {
		return fmt.Errorf("hosts: ensuring %s: %w", topic, err)
	}
	payload := message.GetPayloadCbor()
	if payload == nil {
		payload = []byte{}
	}
	if _, err := queries.InsertTopicMessage(ctx, store.InsertTopicMessageParams{Topic: topic, Payload: payload}); err != nil {
		return fmt.Errorf("hosts: publishing to %s: %w", topic, err)
	}
	return tx.Commit(ctx)
}
