// Package messaging is how the server tells clients and plugins that something
// happened: events in memory, durable topics with a cursor per consumer,
// notifications per user, and the leases plugins take.
package messaging

import (
	"context"
	"errors"
	"sync"
	"time"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
)

// Event is something that happened, about a resource when it is about one.
type Event struct {
	Name     string
	Resource string
	Payload  []byte
	At       time.Time
}

// Subscription is what a subscriber hears, and how it stops.
type Subscription struct {
	// Events is what the subscriber reads, until Close.
	Events <-chan Event
	close  func()
}

// Close ends the subscription.
func (s *Subscription) Close() { s.close() }

// Bus is the events of the leader, in memory: at most once, by name, with a
// prefix match.
type Bus struct {
	mu   sync.Mutex
	next int
	subs map[int]*subscriber
}

// subscriber is one listener's prefix, filter and channel.
type subscriber struct {
	prefix string
	filter func(Event) bool
	events chan Event
}

// NewBus returns an empty bus.
func NewBus() *Bus { return &Bus{subs: make(map[int]*subscriber)} }

// buffer is how many events a subscriber may fall behind before the bus drops
// them. Events are at most once: a client that cannot keep up misses some.
const buffer = 64

// Publish sends an event to every subscriber whose prefix it matches and whose
// filter lets it through.
func (b *Bus) Publish(event Event) {
	if event.At.IsZero() {
		event.At = time.Now()
	}

	b.mu.Lock()
	defer b.mu.Unlock()
	for _, sub := range b.subs {
		if !hasPrefix(event.Name, sub.prefix) {
			continue
		}
		if sub.filter != nil && !sub.filter(event) {
			continue
		}
		select {
		case sub.events <- event:
		default:
			// The subscriber is behind: events are at most once, so it misses
			// this one rather than blocking every publisher.
		}
	}
}

// Subscribe hears the events whose name starts with prefix. A nil filter passes
// everything; the edge's handler passes one that checks the caller may view the
// event's resource.
func (b *Bus) Subscribe(prefix string, filter func(Event) bool) *Subscription {
	b.mu.Lock()
	defer b.mu.Unlock()

	id := b.next
	b.next++
	sub := &subscriber{prefix: prefix, filter: filter, events: make(chan Event, buffer)}
	b.subs[id] = sub

	return &Subscription{
		Events: sub.events,
		close: func() {
			b.mu.Lock()
			defer b.mu.Unlock()
			if _, ok := b.subs[id]; ok {
				delete(b.subs, id)
				close(sub.events)
			}
		},
	}
}

// hasPrefix reports whether an event name starts with a prefix, empty matching
// everything.
func hasPrefix(name, prefix string) bool {
	return prefix == "" || len(name) >= len(prefix) && name[:len(prefix)] == prefix
}

// EventsService is EventService: the events a client hears while it is
// connected.
type EventsService struct {
	bus     *Bus
	checker *access.Checker
}

// NewEventsService returns EventService for a bus and a checker.
func NewEventsService(bus *Bus, checker *access.Checker) *EventsService {
	return &EventsService{bus: bus, checker: checker}
}

// Subscribe streams the events the caller may see.
func (s *EventsService) Subscribe(ctx context.Context, request *connect.Request[rookeryv1.EventServiceSubscribeRequest], stream *connect.ServerStream[rookeryv1.EventServiceSubscribeResponse]) error {
	principal, ok := access.PrincipalFrom(ctx)
	if !ok {
		return access.ToConnect(access.ErrUnauthenticated)
	}

	subscription := s.bus.Subscribe(request.Msg.GetPrefix(), nil)
	defer subscription.Close()

	for {
		select {
		case <-ctx.Done():
			return nil
		case event, ok := <-subscription.Events:
			if !ok {
				return nil
			}
			if event.Resource != "" {
				if err := s.checker.Check(ctx, principal, event.Resource, access.View); err != nil {
					if errors.Is(err, access.ErrDenied) || errors.Is(err, access.ErrInvalid) {
						continue
					}
					return access.ToConnect(err)
				}
			}
			sent := &rookeryv1.Event{
				Name:        event.Name,
				Resource:    event.Resource,
				PayloadCbor: event.Payload,
			}
			if !event.At.IsZero() {
				sent.At = timestamppb.New(event.At)
			}
			if err := stream.Send(&rookeryv1.EventServiceSubscribeResponse{Event: sent}); err != nil {
				return err
			}
		}
	}
}

// Publish is what the server's own parts use to say something happened.
func (s *EventsService) Publish(name, resource string, payload []byte) {
	s.bus.Publish(Event{Name: name, Resource: resource, Payload: payload})
}
