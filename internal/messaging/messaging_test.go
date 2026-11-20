package messaging_test

import (
	"context"
	"errors"
	"net/http"
	"testing"
	"time"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/access/accesstest"
	"github.com/aiviarium/rookery/internal/messaging"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

func TestTheBusMatchesAPrefixAndFilters(t *testing.T) {
	t.Parallel()

	bus := messaging.NewBus()
	all := bus.Subscribe("", nil)
	defer all.Close()
	rookery := bus.Subscribe("rookery/", nil)
	defer rookery.Close()
	onlyForge := bus.Subscribe("", func(event messaging.Event) bool { return event.Resource == access.HostURI("forge") })
	defer onlyForge.Close()

	bus.Publish(messaging.Event{Name: "rookery/jobs.changed", Resource: access.HostURI("forge")})
	bus.Publish(messaging.Event{Name: "example/note.changed"})

	if got := drain(all.Events, 2); len(got) != 2 {
		t.Fatalf("the subscriber of everything heard %d events, want 2", len(got))
	}
	if got := drain(rookery.Events, 1); len(got) != 1 || got[0].Name != "rookery/jobs.changed" {
		t.Fatalf("the rookery subscriber heard %v", got)
	}
	if got := drain(onlyForge.Events, 1); len(got) != 1 || got[0].Resource != access.HostURI("forge") {
		t.Fatalf("the filtered subscriber heard %v", got)
	}
}

// drain reads up to want events, waiting a moment for each.
func drain(events <-chan messaging.Event, want int) []messaging.Event {
	out := make([]messaging.Event, 0, want)
	for len(out) < want {
		select {
		case event := <-events:
			out = append(out, event)
		case <-time.After(time.Second):
			return out
		}
	}
	return out
}

func TestEventsOnlyGoToCallersWhoMaySeeThem(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	checker := access.NewChecker(opened)
	ctx := t.Context()

	bus := messaging.NewBus()
	user := "01halice"
	if _, err := opened.Queries().CreateUser(ctx, store.CreateUserParams{ID: user, Email: "alice@example.test"}); err != nil {
		t.Fatalf("making alice: %v", err)
	}

	server := accesstest.New(t, checker, accesstest.User(user, false), func(mux *http.ServeMux, options []connect.HandlerOption) {
		path, handler := rookeryv1connect.NewEventServiceHandler(messaging.NewEventsService(bus, checker), options...)
		mux.Handle(path, handler)
	})
	client := rookeryv1connect.NewEventServiceClient(server.Client(), server.URL, connect.WithGRPC())

	// A server stream's client call returns with its first message, so the
	// subscriber publishes until one arrives: events are at most once.
	events := make(chan *rookeryv1.Event, 16)
	streamDone := make(chan error, 1)
	go func() {
		stream, err := client.Subscribe(ctx, connect.NewRequest(&rookeryv1.EventServiceSubscribeRequest{Prefix: "rookery/"}))
		if err != nil {
			streamDone <- err
			return
		}
		defer func() { _ = stream.Close() }()
		for stream.Receive() {
			select {
			case events <- stream.Msg().GetEvent():
			default:
			}
		}
		streamDone <- stream.Err()
	}()
	defer func() {
		// The context ends the stream; httptest waits for its handlers.
		_ = streamDone
	}()

	// An event about a host alice may not see is never the first one she gets.
	bus.Publish(messaging.Event{Name: "rookery/hosts.changed", Resource: access.HostURI("forge")})

	var first *rookeryv1.Event
	deadline := time.After(15 * time.Second)
	publish := time.NewTicker(100 * time.Millisecond)
	defer publish.Stop()
	for first == nil {
		select {
		case event := <-events:
			first = event
		case <-publish.C:
			bus.Publish(messaging.Event{Name: "rookery/plugins.changed"})
		case <-deadline:
			t.Fatal("no event arrived")
		}
	}
	if first.GetName() != "rookery/plugins.changed" {
		t.Fatalf("the first event alice heard is %q, which she may not see", first.GetName())
	}

	// Granted view on the host, she hears that one too.
	if _, err := checker.Grant(ctx, access.HostURI("forge"), access.UserPrincipal(user), access.View); err != nil {
		t.Fatalf("granting view: %v", err)
	}
	bus.Publish(messaging.Event{Name: "rookery/hosts.changed", Resource: access.HostURI("forge")})
	deadline = time.After(15 * time.Second)
	for {
		select {
		case event := <-events:
			if event.GetResource() == access.HostURI("forge") {
				return
			}
		case <-publish.C:
			bus.Publish(messaging.Event{Name: "rookery/hosts.changed", Resource: access.HostURI("forge")})
		case <-deadline:
			t.Fatal("the granted event did not arrive")
		}
	}
}

func TestTopicsDeliverAckAndRedeliver(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	topics := messaging.NewTopics(opened)
	ctx := t.Context()

	for _, payload := range []string{"one", "two", "three"} {
		if _, err := topics.Publish(ctx, "example/notes", []byte(payload)); err != nil {
			t.Fatalf("publishing: %v", err)
		}
	}

	deliveries, err := topics.Subscribe(ctx, "example/notes", "workers")
	if err != nil {
		t.Fatalf("subscribing: %v", err)
	}

	first := next(t, deliveries)
	if string(first.Payload) != "one" {
		t.Fatalf("the first message is %q", first.Payload)
	}
	if err := first.Ack(ctx); err != nil {
		t.Fatalf("acknowledging: %v", err)
	}

	second := next(t, deliveries)
	if string(second.Payload) != "two" {
		t.Fatalf("the second message is %q", second.Payload)
	}
	if err := second.Nack(ctx, time.Millisecond, errors.New("not now")); err != nil {
		t.Fatalf("nacking: %v", err)
	}

	third := next(t, deliveries)
	if string(third.Payload) != "three" {
		t.Fatalf("the third message is %q", third.Payload)
	}
	if err := third.Ack(ctx); err != nil {
		t.Fatalf("acknowledging: %v", err)
	}

	// The nacked message comes back, with an attempt counted.
	again := next(t, deliveries)
	if string(again.Payload) != "two" {
		t.Fatalf("the redelivered message is %q", again.Payload)
	}
	if again.Attempts != 1 {
		t.Errorf("the redelivered message has %d attempts, want 1", again.Attempts)
	}
	if err := again.Ack(ctx); err != nil {
		t.Fatalf("acknowledging: %v", err)
	}
}

func TestATopicMessageBecomesADeadLetter(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	topics := messaging.NewTopics(opened)
	ctx := t.Context()

	if _, err := topics.Publish(ctx, "example/notes", []byte("poison")); err != nil {
		t.Fatalf("publishing: %v", err)
	}

	deliveries, err := topics.Subscribe(ctx, "example/notes", "workers")
	if err != nil {
		t.Fatalf("subscribing: %v", err)
	}
	for attempt := 1; attempt <= messaging.MaxAttempts; attempt++ {
		delivery := next(t, deliveries)
		if err := delivery.Nack(ctx, time.Millisecond, errors.New("it is poison")); err != nil {
			t.Fatalf("nacking attempt %d: %v", attempt, err)
		}
	}

	messages, letters, err := topics.Inspect(ctx, "example/notes", "", 10)
	if err != nil {
		t.Fatalf("inspecting: %v", err)
	}
	if len(messages) != 0 {
		t.Fatalf("the dead message is still on the topic: %d", len(messages))
	}
	if len(letters) != 1 {
		t.Fatalf("there are %d dead letters, want 1", len(letters))
	}
	if letters[0].Consumer != "workers" || !errors.Is(err, nil) {
		t.Errorf("the dead letter is %+v", letters[0])
	}

	// Retrying puts it back on the topic and drops the dead letter.
	if err := topics.Retry(ctx, letters[0].ID); err != nil {
		t.Fatalf("retrying: %v", err)
	}
	messages, letters, err = topics.Inspect(ctx, "example/notes", "", 10)
	if err != nil {
		t.Fatalf("inspecting: %v", err)
	}
	if len(messages) != 1 || len(letters) != 0 {
		t.Fatalf("after a retry there are %d messages and %d dead letters", len(messages), len(letters))
	}
}

// next reads the next delivery, or fails the test.
func next(t *testing.T, deliveries <-chan messaging.Delivery) messaging.Delivery {
	t.Helper()

	select {
	case delivery, ok := <-deliveries:
		if !ok {
			t.Fatal("the subscription ended")
		}
		return delivery
	case <-time.After(10 * time.Second):
		t.Fatal("no message arrived")
	}
	return messaging.Delivery{}
}

func TestNotificationsAreAUsersOwn(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	ctx := t.Context()

	alice := "01halice"
	bob := "01hbob"
	for _, user := range []string{alice, bob} {
		if _, err := opened.Queries().CreateUser(ctx, store.CreateUserParams{ID: user, Email: user + "@example.test"}); err != nil {
			t.Fatalf("making %s: %v", user, err)
		}
	}

	notifications := messaging.NewNotifications(opened)
	sent, err := notifications.Send(ctx, messaging.Notification{
		UserID:   alice,
		Title:    "The apply failed",
		Body:     "forge could not be reached",
		Resource: access.HostURI("forge"),
	})
	if err != nil {
		t.Fatalf("sending: %v", err)
	}

	listed, err := notifications.List(ctx, alice, "", false, 10)
	if err != nil {
		t.Fatalf("listing: %v", err)
	}
	if len(listed) != 1 || listed[0].Title != "The apply failed" {
		t.Fatalf("alice has %+v", listed)
	}
	if listed, err := notifications.List(ctx, bob, "", false, 10); err != nil {
		t.Fatalf("listing bob's: %v", err)
	} else if len(listed) != 0 {
		t.Fatalf("bob has %d notifications", len(listed))
	}

	if _, err := notifications.MarkRead(ctx, bob, sent.ID, true); !errors.Is(err, access.ErrNotFound) {
		t.Fatalf("bob marked alice's notification: %v", err)
	}
	read, err := notifications.MarkRead(ctx, alice, sent.ID, true)
	if err != nil {
		t.Fatalf("marking read: %v", err)
	}
	if !read.Read {
		t.Fatal("the notification is not read")
	}

	answered, err := notifications.Answer(ctx, alice, sent.ID, "retry", []byte("now"))
	if err != nil {
		t.Fatalf("answering: %v", err)
	}
	if len(answered.Answers) == 0 {
		t.Fatal("the answer was not recorded")
	}

	if err := notifications.SetPreferences(ctx, alice, []byte("prefs")); err != nil {
		t.Fatalf("setting preferences: %v", err)
	}
	prefs, found, err := notifications.Preferences(ctx, alice)
	if err != nil {
		t.Fatalf("reading preferences: %v", err)
	}
	if !found || string(prefs) != "prefs" {
		t.Fatalf("the preferences are %q", prefs)
	}
}

func TestANotificationWatchSeesNewOnes(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	ctx := t.Context()
	alice := "01halice"
	if _, err := opened.Queries().CreateUser(ctx, store.CreateUserParams{ID: alice, Email: "alice@example.test"}); err != nil {
		t.Fatalf("making alice: %v", err)
	}

	notifications := messaging.NewNotifications(opened)
	watchCtx, cancel := context.WithCancel(ctx)
	defer cancel()

	watched, err := notifications.Watch(watchCtx, alice, "")
	if err != nil {
		t.Fatalf("watching: %v", err)
	}
	if _, err := notifications.Send(ctx, messaging.Notification{UserID: alice, Title: "A job failed"}); err != nil {
		t.Fatalf("sending: %v", err)
	}

	select {
	case notification, ok := <-watched:
		if !ok {
			t.Fatal("the watch ended")
		}
		if notification.Title != "A job failed" {
			t.Fatalf("the watch saw %q", notification.Title)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("the watch saw nothing")
	}
}

func TestLeasesFenceTheirHolders(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	leases := messaging.NewLeases(opened)
	ctx := t.Context()

	first, err := leases.Acquire(ctx, "nightly", "holder-a", time.Minute)
	if err != nil {
		t.Fatalf("acquiring: %v", err)
	}
	if first.Fencing != 1 {
		t.Errorf("the first acquire has fencing %d", first.Fencing)
	}

	if _, err := leases.Acquire(ctx, "nightly", "holder-b", time.Minute); !errors.Is(err, access.ErrConflict) {
		t.Fatalf("a second holder took the lease: %v", err)
	}

	second, err := leases.Acquire(ctx, "nightly", "holder-a", time.Minute)
	if err != nil {
		t.Fatalf("renewing by acquiring: %v", err)
	}
	if second.Fencing != 2 {
		t.Errorf("the second acquire has fencing %d, want 2", second.Fencing)
	}

	if err := leases.Renew(ctx, "nightly", "holder-a", second.Fencing, time.Minute); err != nil {
		t.Fatalf("renewing: %v", err)
	}
	if err := leases.Renew(ctx, "nightly", "holder-a", first.Fencing, time.Minute); !errors.Is(err, access.ErrConflict) {
		t.Fatalf("a stale fencing number renewed the lease: %v", err)
	}

	if err := leases.Release(ctx, "nightly", "holder-a", second.Fencing); err != nil {
		t.Fatalf("releasing: %v", err)
	}
	if _, err := leases.Acquire(ctx, "nightly", "holder-b", time.Minute); err != nil {
		t.Fatalf("acquiring after a release: %v", err)
	}
}
