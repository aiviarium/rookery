package messaging

import (
	"context"
	"errors"
	"fmt"
	"math"
	"strings"
	"time"

	"connectrpc.com/connect"
	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgtype"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/store"
)

// Defaults of topics.
const (
	// MaxAttempts is how often a message is delivered before it is a dead
	// letter.
	MaxAttempts = 5
	// NackDelay is how long a message waits after a nack when the caller names
	// no delay.
	NackDelay = 30 * time.Second
	// pollInterval is how often a subscription looks for a message.
	pollInterval = 250 * time.Millisecond
	// batchSize is how many messages a subscription reads at once.
	batchSize = 16
)

// Topics are durable messages, with a cursor per consumer group.
type Topics struct {
	store *store.Store
}

// NewTopics returns the topics of a database.
func NewTopics(opened *store.Store) *Topics { return &Topics{store: opened} }

// Message is one topic message.
type Message struct {
	ID        int64
	Topic     string
	Payload   []byte
	Attempts  int
	CreatedAt time.Time
}

// DeadLetter is a message a consumer gave up on.
type DeadLetter struct {
	ID       int64
	Topic    string
	Consumer string
	Payload  []byte
	Attempts int
	Error    string
	At       time.Time
}

// Publish puts a message on a topic, and returns its id.
func (t *Topics) Publish(ctx context.Context, topic string, payload []byte) (int64, error) {
	topic = trimTopic(topic)
	if topic == "" {
		return 0, fmt.Errorf("%w: a message needs a topic", access.ErrInvalid)
	}
	if payload == nil {
		payload = []byte{}
	}

	tx, err := t.store.Pool().Begin(ctx)
	if err != nil {
		return 0, fmt.Errorf("messaging: beginning the publish: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	queries := t.store.Queries().WithTx(tx)
	if err := queries.EnsureTopic(ctx, topic); err != nil {
		return 0, fmt.Errorf("messaging: recording the topic %s: %w", topic, err)
	}
	message, err := queries.InsertTopicMessage(ctx, store.InsertTopicMessageParams{Topic: topic, Payload: payload})
	if err != nil {
		return 0, fmt.Errorf("messaging: publishing to %s: %w", topic, err)
	}
	if err := tx.Commit(ctx); err != nil {
		return 0, fmt.Errorf("messaging: committing the publish: %w", err)
	}
	return message.ID, nil
}

// Delivery is a message handed to a consumer, with the two ways to end it.
type Delivery struct {
	Message
	// Ack takes the message out of the topic for the consumer group.
	Ack func(ctx context.Context) error
	// Nack asks for another delivery after delay, or for a dead letter when the
	// message has had its attempts.
	Nack func(ctx context.Context, delay time.Duration, cause error) error
}

// Subscribe delivers a topic's messages to a consumer group, from its cursor on.
func (t *Topics) Subscribe(ctx context.Context, topic, consumer string) (<-chan Delivery, error) {
	topic, consumer = trimTopic(topic), trimTopic(consumer)
	if topic == "" || consumer == "" {
		return nil, fmt.Errorf("%w: a subscription needs a topic and a consumer name", access.ErrInvalid)
	}
	if _, err := t.store.Queries().CursorFor(ctx, store.CursorForParams{Topic: topic, Consumer: consumer}); err != nil {
		return nil, fmt.Errorf("messaging: reading the cursor of %s on %s: %w", consumer, topic, err)
	}

	deliveries := make(chan Delivery)
	go func() {
		defer close(deliveries)
		t.deliver(ctx, topic, consumer, deliveries)
	}()
	return deliveries, nil
}

// deliver is the subscription's loop: read the messages after the cursor, hand
// them over, and look again.
func (t *Topics) deliver(ctx context.Context, topic, consumer string, deliveries chan<- Delivery) {
	ticker := time.NewTicker(pollInterval)
	defer ticker.Stop()

	var cursor int64
	for {
		state, err := t.store.Queries().CursorFor(ctx, store.CursorForParams{Topic: topic, Consumer: consumer})
		if err != nil {
			return
		}
		cursor = state.NextSeq

		messages, err := t.store.Queries().TopicMessagesAfter(ctx, store.TopicMessagesAfterParams{
			Topic: topic,
			ID:    cursor,
			Limit: batchSize,
		})
		if err != nil {
			return
		}

		for _, record := range messages {
			delivery := Delivery{
				Message: messageOf(record),
				Ack: func(ctx context.Context) error {
					return t.ack(ctx, topic, consumer, record.ID)
				},
				Nack: func(ctx context.Context, delay time.Duration, cause error) error {
					return t.nack(ctx, topic, consumer, record, delay, cause)
				},
			}
			select {
			case <-ctx.Done():
				return
			case deliveries <- delivery:
			}
		}

		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}
}

// ack moves the consumer group's cursor past a message.
func (t *Topics) ack(ctx context.Context, topic, consumer string, messageID int64) error {
	if err := t.store.Queries().AdvanceCursor(ctx, store.AdvanceCursorParams{
		Topic:    topic,
		Consumer: consumer,
		NextSeq:  messageID + 1,
	}); err != nil {
		return fmt.Errorf("messaging: acknowledging %d on %s: %w", messageID, topic, err)
	}
	return nil
}

// nack asks for another delivery, or makes a dead letter of a message that has
// had its attempts.
//
// A redelivery is a new message past the cursor with the attempt counted and
// its next delivery deferred: one cursor per consumer group cannot hold a
// message that waits while later ones are acknowledged, so the message moves to
// the end of the topic instead.
func (t *Topics) nack(ctx context.Context, topic, consumer string, record store.RookeryTopicMessages, delay time.Duration, cause error) error {
	if delay <= 0 {
		delay = NackDelay
	}
	attempts := int(record.Attempts) + 1

	if attempts >= MaxAttempts {
		why := ""
		if cause != nil {
			why = cause.Error()
		}
		if _, err := t.store.Queries().InsertDeadLetter(ctx, store.InsertDeadLetterParams{
			Topic:    topic,
			Consumer: consumer,
			Payload:  record.Payload,
			Attempts: int32(min(attempts, math.MaxInt32)), //nolint:gosec // clamped to int32
			Error:    why,
		}); err != nil {
			return fmt.Errorf("messaging: making a dead letter of %d on %s: %w", record.ID, topic, err)
		}
	} else {
		if _, err := t.store.Queries().RedeliverTopicMessage(ctx, store.RedeliverTopicMessageParams{
			Topic:    topic,
			Payload:  record.Payload,
			Attempts: int32(min(attempts, math.MaxInt32)), //nolint:gosec // clamped to int32
			Delay:    interval(delay),
		}); err != nil {
			return fmt.Errorf("messaging: redelivering %d on %s: %w", record.ID, topic, err)
		}
	}

	if _, err := t.store.Queries().DeleteTopicMessage(ctx, record.ID); err != nil {
		return fmt.Errorf("messaging: removing the handled message %d: %w", record.ID, err)
	}
	return t.ack(ctx, topic, consumer, record.ID)
}

// Inspect lists a topic's messages and its dead letters.
func (t *Topics) Inspect(ctx context.Context, topic, consumer string, limit int) ([]Message, []DeadLetter, error) {
	var messages []Message
	if topic != "" {
		records, err := t.store.Queries().TopicMessagesForInspect(ctx, store.TopicMessagesForInspectParams{
			Topic: topic,
			Limit: clampLimit(limit),
		})
		if err != nil {
			return nil, nil, fmt.Errorf("messaging: inspecting %s: %w", topic, err)
		}
		for _, record := range records {
			messages = append(messages, messageOf(record))
		}
	}

	dead, err := t.store.Queries().ListDeadLetters(ctx, store.ListDeadLettersParams{
		Topic:    optional(topic),
		Consumer: optional(consumer),
		RowLimit: clampLimit(limit),
	})
	if err != nil {
		return nil, nil, fmt.Errorf("messaging: reading the dead letters: %w", err)
	}
	letters := make([]DeadLetter, 0, len(dead))
	for _, record := range dead {
		letters = append(letters, deadLetterOf(record))
	}
	return messages, letters, nil
}

// Retry puts a dead letter back on its topic, for its consumer group to take
// again.
func (t *Topics) Retry(ctx context.Context, deadLetterID int64) error {
	letter, err := t.deadLetter(ctx, deadLetterID)
	if err != nil {
		return err
	}
	if _, err := t.Publish(ctx, letter.Topic, letter.Payload); err != nil {
		return err
	}
	if _, err := t.store.Queries().DeleteDeadLetter(ctx, deadLetterID); err != nil {
		return fmt.Errorf("messaging: removing the dead letter %d: %w", deadLetterID, err)
	}
	return nil
}

// Discard drops a dead letter.
func (t *Topics) Discard(ctx context.Context, deadLetterID int64) error {
	_, err := t.store.Queries().DeleteDeadLetter(ctx, deadLetterID)
	if err != nil {
		return fmt.Errorf("messaging: discarding the dead letter %d: %w", deadLetterID, err)
	}
	return nil
}

// deadLetter reads one.
func (t *Topics) deadLetter(ctx context.Context, deadLetterID int64) (DeadLetter, error) {
	record, err := t.store.Queries().GetDeadLetter(ctx, deadLetterID)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return DeadLetter{}, fmt.Errorf("%w: no dead letter %d", access.ErrNotFound, deadLetterID)
	case err != nil:
		return DeadLetter{}, fmt.Errorf("messaging: reading the dead letter %d: %w", deadLetterID, err)
	}
	return deadLetterOf(record), nil
}

// messageOf is a stored message.
func messageOf(record store.RookeryTopicMessages) Message {
	return Message{
		ID:        record.ID,
		Topic:     record.Topic,
		Payload:   record.Payload,
		Attempts:  int(record.Attempts),
		CreatedAt: record.CreatedAt,
	}
}

// deadLetterOf is a stored dead letter.
func deadLetterOf(record store.RookeryTopicDead) DeadLetter {
	return DeadLetter{
		ID:       record.ID,
		Topic:    record.Topic,
		Consumer: record.Consumer,
		Payload:  record.Payload,
		Attempts: int(record.Attempts),
		Error:    record.Error,
		At:       record.At,
	}
}

// TopicsService is TopicService.
type TopicsService struct {
	topics *Topics
}

// NewTopicsService returns TopicService for a database.
func NewTopicsService(topics *Topics) *TopicsService { return &TopicsService{topics: topics} }

// Publish puts a message on a topic.
func (s *TopicsService) Publish(ctx context.Context, request *connect.Request[rookeryv1.TopicServicePublishRequest]) (*connect.Response[rookeryv1.TopicServicePublishResponse], error) {
	id, err := s.topics.Publish(ctx, request.Msg.GetTopic(), request.Msg.GetPayloadCbor())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.TopicServicePublishResponse{Id: id}), nil
}

// Subscribe streams a topic's messages to a consumer group.
func (s *TopicsService) Subscribe(ctx context.Context, request *connect.Request[rookeryv1.TopicServiceSubscribeRequest], stream *connect.ServerStream[rookeryv1.TopicServiceSubscribeResponse]) error {
	deliveries, err := s.topics.Subscribe(ctx, request.Msg.GetTopic(), request.Msg.GetConsumer())
	if err != nil {
		return access.ToConnect(err)
	}

	for {
		select {
		case <-ctx.Done():
			return nil
		case delivery, ok := <-deliveries:
			if !ok {
				return nil
			}
			message := &rookeryv1.TopicMessage{
				Id:          delivery.ID,
				Topic:       delivery.Topic,
				PayloadCbor: delivery.Payload,
				Attempts:    int32(min(delivery.Attempts, math.MaxInt32)), //nolint:gosec // clamped to int32
			}
			if !delivery.CreatedAt.IsZero() {
				message.CreatedAt = timestamppb.New(delivery.CreatedAt)
			}
			if err := stream.Send(&rookeryv1.TopicServiceSubscribeResponse{Message: message}); err != nil {
				return err
			}
		}
	}
}

// Ack acknowledges a message.
func (s *TopicsService) Ack(ctx context.Context, request *connect.Request[rookeryv1.TopicServiceAckRequest]) (*connect.Response[rookeryv1.TopicServiceAckResponse], error) {
	if err := s.topics.ack(ctx, trimTopic(request.Msg.GetTopic()), consumerOf(request.Msg.GetConsumer()), request.Msg.GetId()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.TopicServiceAckResponse{}), nil
}

// Nack asks for another delivery.
func (s *TopicsService) Nack(ctx context.Context, request *connect.Request[rookeryv1.TopicServiceNackRequest]) (*connect.Response[rookeryv1.TopicServiceNackResponse], error) {
	var cause error
	if request.Msg.GetError() != "" {
		cause = errors.New(request.Msg.GetError())
	}
	record, err := s.topics.store.Queries().TopicMessage(ctx, request.Msg.GetId())
	if err != nil {
		return nil, access.ToConnect(fmt.Errorf("messaging: reading the message %d: %w", request.Msg.GetId(), err))
	}
	if err := s.topics.nack(ctx, trimTopic(request.Msg.GetTopic()), consumerOf(request.Msg.GetConsumer()), record, time.Duration(request.Msg.GetDelaySeconds())*time.Second, cause); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.TopicServiceNackResponse{}), nil
}

// Inspect lists a topic's messages and dead letters.
func (s *TopicsService) Inspect(ctx context.Context, request *connect.Request[rookeryv1.TopicServiceInspectRequest]) (*connect.Response[rookeryv1.TopicServiceInspectResponse], error) {
	messages, letters, err := s.topics.Inspect(ctx, request.Msg.GetTopic(), request.Msg.GetConsumer(), int(request.Msg.GetLimit()))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := &rookeryv1.TopicServiceInspectResponse{}
	for _, message := range messages {
		converted := &rookeryv1.TopicMessage{
			Id:          message.ID,
			Topic:       message.Topic,
			PayloadCbor: message.Payload,
			Attempts:    int32(min(message.Attempts, math.MaxInt32)), //nolint:gosec // clamped to int32
		}
		if !message.CreatedAt.IsZero() {
			converted.CreatedAt = timestamppb.New(message.CreatedAt)
		}
		out.Messages = append(out.Messages, converted)
	}
	for _, letter := range letters {
		converted := &rookeryv1.TopicDeadLetter{
			Id:          letter.ID,
			Topic:       letter.Topic,
			Consumer:    letter.Consumer,
			PayloadCbor: letter.Payload,
			Attempts:    int32(min(letter.Attempts, math.MaxInt32)), //nolint:gosec // clamped to int32
		}
		if !letter.At.IsZero() {
			converted.At = timestamppb.New(letter.At)
		}
		out.DeadLetters = append(out.DeadLetters, converted)
	}
	return connect.NewResponse(out), nil
}

// Retry puts a dead letter back on its topic.
func (s *TopicsService) Retry(ctx context.Context, request *connect.Request[rookeryv1.TopicServiceRetryRequest]) (*connect.Response[rookeryv1.TopicServiceRetryResponse], error) {
	if err := s.topics.Retry(ctx, request.Msg.GetDeadLetter()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.TopicServiceRetryResponse{}), nil
}

// Discard drops a dead letter.
func (s *TopicsService) Discard(ctx context.Context, request *connect.Request[rookeryv1.TopicServiceDiscardRequest]) (*connect.Response[rookeryv1.TopicServiceDiscardResponse], error) {
	if err := s.topics.Discard(ctx, request.Msg.GetDeadLetter()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.TopicServiceDiscardResponse{}), nil
}

// trimTopic is a topic's name without spaces around it.
func trimTopic(topic string) string { return strings.TrimSpace(topic) }

// consumerOf is a consumer group's name, defaulting to "edge" for a caller that
// names none.
func consumerOf(consumer string) string {
	if consumer = strings.TrimSpace(consumer); consumer == "" {
		return "edge"
	}
	return consumer
}

// interval is a duration as a Postgres interval.
func interval(duration time.Duration) pgtype.Interval {
	return pgtype.Interval{Microseconds: duration.Microseconds(), Valid: true}
}

// optional is a filter value, or nil for no filter.
func optional(value string) *string {
	if value == "" {
		return nil
	}
	return &value
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
