package messaging

import (
	"context"
	"errors"
	"fmt"
	"time"

	"connectrpc.com/connect"
	"github.com/jackc/pgx/v5"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/id"
	"github.com/aiviarium/rookery/internal/store"
)

// watchInterval is how often a notification watch looks for new ones.
const watchInterval = 500 * time.Millisecond

// Notification is one thing a user is told about.
type Notification struct {
	ID        string
	UserID    string
	Title     string
	Body      string
	Link      string
	Resource  string
	Actions   []byte
	Answers   []byte
	Read      bool
	CreatedAt time.Time
}

// Message is a notification as the edge API has it.
func (n Notification) Message() *rookeryv1.Notification {
	out := &rookeryv1.Notification{
		Id:          n.ID,
		UserId:      n.UserID,
		Title:       n.Title,
		Body:        n.Body,
		Link:        n.Link,
		Resource:    n.Resource,
		ActionsCbor: n.Actions,
		AnswersCbor: n.Answers,
		Read:        n.Read,
	}
	if !n.CreatedAt.IsZero() {
		out.CreatedAt = timestamppb.New(n.CreatedAt)
	}
	return out
}

// notificationOf is a notification from the database.
func notificationOf(record store.RookeryNotifications) Notification {
	notification := Notification{
		ID:       record.ID,
		UserID:   record.UserID,
		Title:    record.Title,
		Body:     record.Body,
		Link:     record.Link,
		Resource: record.Resource,
		Actions:  record.Actions,
		Answers:  record.Answers,
		Read:     record.ReadAt != nil,
	}
	if !record.CreatedAt.IsZero() {
		notification.CreatedAt = record.CreatedAt
	}
	return notification
}

// Notifications are the messages the server and plugins send a user.
type Notifications struct {
	store *store.Store
	// changed tells a watch that a notification arrived, so it does not wait
	// for its next look.
	changed chan struct{}
}

// NewNotifications returns the notifications of a database.
func NewNotifications(opened *store.Store) *Notifications {
	return &Notifications{store: opened, changed: make(chan struct{}, 1)}
}

// Send tells a user something. The title is required; everything else is the
// caller's.
func (n *Notifications) Send(ctx context.Context, notification Notification) (Notification, error) {
	if notification.UserID == "" {
		return Notification{}, fmt.Errorf("%w: a notification names the user it is for", access.ErrInvalid)
	}
	if notification.Title == "" {
		return Notification{}, fmt.Errorf("%w: a notification needs a title", access.ErrInvalid)
	}
	if notification.Resource != "" {
		if _, err := access.ParseResource(notification.Resource); err != nil {
			return Notification{}, err
		}
	}

	record, err := n.store.Queries().CreateNotification(ctx, store.CreateNotificationParams{
		ID:       id.New(),
		UserID:   notification.UserID,
		Title:    notification.Title,
		Body:     notification.Body,
		Link:     notification.Link,
		Resource: notification.Resource,
		Actions:  notification.Actions,
	})
	if err != nil {
		return Notification{}, fmt.Errorf("messaging: sending a notification: %w", err)
	}
	n.notify()
	return notificationOf(record), nil
}

// List is a page of a user's notifications, newest first.
func (n *Notifications) List(ctx context.Context, userID, cursor string, unreadOnly bool, limit int) ([]Notification, error) {
	if cursor == "" {
		cursor = "~"
	}
	records, err := n.store.Queries().ListNotifications(ctx, store.ListNotificationsParams{
		UserID:     userID,
		UnreadOnly: unreadOnly,
		Cursor:     cursor,
		RowLimit:   clampLimit(limit),
	})
	if err != nil {
		return nil, fmt.Errorf("messaging: listing notifications: %w", err)
	}
	out := make([]Notification, 0, len(records))
	for _, record := range records {
		out = append(out, notificationOf(record))
	}
	return out, nil
}

// Watch streams a user's new notifications, from the one named on, until ctx
// ends.
func (n *Notifications) Watch(ctx context.Context, userID, from string) (<-chan Notification, error) {
	// A watch that names no notification starts with the newest one there is,
	// so that a caller hears what arrives after it connected.
	after := from
	if after == "" {
		newest, err := n.store.Queries().NewestNotification(ctx, userID)
		if err != nil {
			return nil, fmt.Errorf("messaging: reading the newest notification of %s: %w", userID, err)
		}
		after = newest
	}

	out := make(chan Notification)

	go func() {
		defer close(out)
		ticker := time.NewTicker(watchInterval)
		defer ticker.Stop()

		for {
			records, err := n.store.Queries().NotificationsAfter(ctx, store.NotificationsAfterParams{
				UserID: userID,
				ID:     after,
				Limit:  batchSize,
			})
			if err != nil {
				return
			}
			for _, record := range records {
				after = record.ID
				select {
				case <-ctx.Done():
					return
				case out <- notificationOf(record):
				}
			}

			select {
			case <-ctx.Done():
				return
			case <-n.changed:
			case <-ticker.C:
			}
		}
	}()
	return out, nil
}

// MarkRead marks one of a user's notifications read, or unread.
func (n *Notifications) MarkRead(ctx context.Context, userID, notificationID string, read bool) (Notification, error) {
	if _, err := n.owned(ctx, userID, notificationID); err != nil {
		return Notification{}, err
	}
	record, err := n.store.Queries().MarkNotificationRead(ctx, store.MarkNotificationReadParams{
		ID:   notificationID,
		Read: read,
	})
	if err != nil {
		return Notification{}, fmt.Errorf("messaging: marking %s read: %w", notificationID, err)
	}
	return notificationOf(record), nil
}

// Answer records what a user answered to a notification's actions.
func (n *Notifications) Answer(ctx context.Context, userID, notificationID, action string, value []byte) (Notification, error) {
	current, err := n.owned(ctx, userID, notificationID)
	if err != nil {
		return Notification{}, err
	}

	answers := make(map[string][]byte)
	if len(current.Answers) > 0 {
		if err := cborx.Unmarshal(current.Answers, &answers); err != nil {
			return Notification{}, fmt.Errorf("messaging: reading the answers of %s: %w", notificationID, err)
		}
	}
	if value == nil {
		value = []byte{}
	}
	answers[action] = value
	encoded, err := cborx.Marshal(answers)
	if err != nil {
		return Notification{}, err
	}

	record, err := n.store.Queries().AnswerNotification(ctx, store.AnswerNotificationParams{
		Answers: encoded,
		ID:      notificationID,
	})
	if err != nil {
		return Notification{}, fmt.Errorf("messaging: answering %s: %w", notificationID, err)
	}
	return notificationOf(record), nil
}

// SetPreferences saves a user's notification preferences.
func (n *Notifications) SetPreferences(ctx context.Context, userID string, preferences []byte) error {
	if preferences == nil {
		preferences = []byte{}
	}
	if err := n.store.Queries().SetNotificationPrefs(ctx, store.SetNotificationPrefsParams{
		UserID: userID,
		Prefs:  preferences,
	}); err != nil {
		return fmt.Errorf("messaging: saving the preferences of %s: %w", userID, err)
	}
	return nil
}

// Preferences returns a user's notification preferences, and false when they
// have set none.
func (n *Notifications) Preferences(ctx context.Context, userID string) ([]byte, bool, error) {
	record, err := n.store.Queries().GetNotificationPrefs(ctx, userID)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return nil, false, nil
	case err != nil:
		return nil, false, fmt.Errorf("messaging: reading the preferences of %s: %w", userID, err)
	}
	return record.Prefs, true, nil
}

// owned reads a notification and checks it is the user's.
func (n *Notifications) owned(ctx context.Context, userID, notificationID string) (Notification, error) {
	record, err := n.store.Queries().GetNotification(ctx, notificationID)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return Notification{}, fmt.Errorf("%w: no notification %s", access.ErrNotFound, notificationID)
	case err != nil:
		return Notification{}, fmt.Errorf("messaging: reading the notification %s: %w", notificationID, err)
	case record.UserID != userID:
		return Notification{}, fmt.Errorf("%w: the notification is somebody else's", access.ErrNotFound)
	}
	return notificationOf(record), nil
}

// notify wakes a watch.
func (n *Notifications) notify() {
	select {
	case n.changed <- struct{}{}:
	default:
	}
}

// NotificationsService is NotificationService: a user's own notifications.
type NotificationsService struct {
	notifications *Notifications
}

// NewNotificationsService returns NotificationService for a database.
func NewNotificationsService(notifications *Notifications) *NotificationsService {
	return &NotificationsService{notifications: notifications}
}

// List returns a page of the caller's notifications.
func (s *NotificationsService) List(ctx context.Context, request *connect.Request[rookeryv1.NotificationServiceListRequest]) (*connect.Response[rookeryv1.NotificationServiceListResponse], error) {
	principal := access.MustPrincipal(ctx)
	found, err := s.notifications.List(ctx, principal.ID, request.Msg.GetCursor(), request.Msg.GetUnreadOnly(), int(request.Msg.GetLimit()))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := make([]*rookeryv1.Notification, 0, len(found))
	for _, notification := range found {
		out = append(out, notification.Message())
	}
	next := ""
	limit := int(request.Msg.GetLimit())
	if limit <= 0 {
		limit = 100
	}
	if len(found) == limit {
		next = found[len(found)-1].ID
	}
	return connect.NewResponse(&rookeryv1.NotificationServiceListResponse{Notifications: out, NextCursor: next}), nil
}

// Watch streams the caller's new notifications.
func (s *NotificationsService) Watch(ctx context.Context, request *connect.Request[rookeryv1.NotificationServiceWatchRequest], stream *connect.ServerStream[rookeryv1.NotificationServiceWatchResponse]) error {
	principal := access.MustPrincipal(ctx)
	watched, err := s.notifications.Watch(ctx, principal.ID, request.Msg.GetFrom())
	if err != nil {
		return access.ToConnect(err)
	}

	for {
		select {
		case <-ctx.Done():
			return nil
		case notification, ok := <-watched:
			if !ok {
				return nil
			}
			if err := stream.Send(&rookeryv1.NotificationServiceWatchResponse{Notification: notification.Message()}); err != nil {
				return err
			}
		}
	}
}

// MarkRead marks one of the caller's notifications read or unread.
func (s *NotificationsService) MarkRead(ctx context.Context, request *connect.Request[rookeryv1.NotificationServiceMarkReadRequest]) (*connect.Response[rookeryv1.NotificationServiceMarkReadResponse], error) {
	principal := access.MustPrincipal(ctx)
	notification, err := s.notifications.MarkRead(ctx, principal.ID, request.Msg.GetNotification(), request.Msg.GetRead())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.NotificationServiceMarkReadResponse{Notification: notification.Message()}), nil
}

// Answer records what the caller chose.
func (s *NotificationsService) Answer(ctx context.Context, request *connect.Request[rookeryv1.NotificationServiceAnswerRequest]) (*connect.Response[rookeryv1.NotificationServiceAnswerResponse], error) {
	principal := access.MustPrincipal(ctx)
	if request.Msg.GetAction() == "" {
		return nil, access.ToConnect(fmt.Errorf("%w: an answer names an action", access.ErrInvalid))
	}
	notification, err := s.notifications.Answer(ctx, principal.ID, request.Msg.GetNotification(), request.Msg.GetAction(), request.Msg.GetValueCbor())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.NotificationServiceAnswerResponse{Notification: notification.Message()}), nil
}

// SetPreference saves the caller's preferences.
func (s *NotificationsService) SetPreference(ctx context.Context, request *connect.Request[rookeryv1.NotificationServiceSetPreferenceRequest]) (*connect.Response[rookeryv1.NotificationServiceSetPreferenceResponse], error) {
	principal := access.MustPrincipal(ctx)
	if err := s.notifications.SetPreferences(ctx, principal.ID, request.Msg.GetPrefsCbor()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.NotificationServiceSetPreferenceResponse{}), nil
}
