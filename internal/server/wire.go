package server

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"time"

	"golang.org/x/sync/errgroup"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/audit"
	"github.com/aiviarium/rookery/internal/authn"
	"github.com/aiviarium/rookery/internal/edge"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/messaging"
	"github.com/aiviarium/rookery/internal/seal"
	"github.com/aiviarium/rookery/internal/spaces"
	"github.com/aiviarium/rookery/internal/store"
)

// wire builds every service and both listeners' handlers. Every dependency goes
// in here, in one place, in the order start-up needs it.
func (s *Server) wire(ctx context.Context, opened *store.Store) (*handlers, error) {
	ring, err := seal.Load(s.config.SecretKey, s.config.SecretKeyFile, true)
	if err != nil {
		return nil, err
	}
	if _, err := seal.NewRing(ring.ID(), ring); err != nil {
		return nil, err
	}
	// The ring is what the secrets, settings and connections item seals with.
	s.log.Debug("the sealing key is loaded", slog.String("key_id", ring.ID()))

	checker := access.NewChecker(opened)
	recorder := audit.NewRecorder(opened)

	users := authn.NewUsers(opened)
	sessions := authn.NewSessions(opened)
	tokens := authn.NewTokens(opened)
	links := authn.NewLinks(opened, s.config.PublicURL)
	groups := authn.NewGroups(opened)
	spaceList := spaces.New(opened, checker)

	var provider *authn.OIDC
	if s.config.OIDCEnabled() {
		provider, err = authn.Discover(ctx, authn.OIDCConfig{
			Issuer:       s.config.OIDCIssuer,
			ClientID:     s.config.OIDCClientID,
			ClientSecret: s.config.OIDCClientSecret,
			RedirectURL:  authn.CallbackURL(s.config.PublicURL),
			GroupsClaim:  s.config.OIDCGroupsClaim,
			Admins:       s.config.Admins,
		})
		if err != nil {
			return nil, err
		}
	}
	if provider == nil && len(s.config.Admins) > 0 {
		s.log.Warn("ROOKERY_ADMINS has no effect without OIDC")
	}

	interceptors := []connect.Interceptor{
		audit.NewInterceptor(recorder, s.log),
		access.NewInterceptor(checker),
	}

	// Jobs and messaging: the bus is published to by the runner, and the
	// runner's jobs are followed by the service.
	bus := messaging.NewBus()
	events := messaging.NewEventsService(bus, checker)
	topics := messaging.NewTopicsService(messaging.NewTopics(opened))
	notifications := messaging.NewNotificationsService(messaging.NewNotifications(opened))

	registry := jobs.NewRegistry()
	if err := registry.RegisterKinds(jobs.CoreKinds()...); err != nil {
		return nil, err
	}
	runner := jobs.NewRunner(opened, registry, events, s.log)
	jobRunner := jobs.NewService(opened, checker, registry, runner, s.log)
	schedules := jobs.NewSchedules(opened, registry, jobRunner, s.log)
	scheduler := jobs.NewScheduler(schedules)

	if interrupted, err := jobs.InterruptRunning(ctx, opened); err != nil {
		return nil, err
	} else if interrupted > 0 {
		s.log.Info("interrupted the jobs a stopped server left running", slog.Int64("jobs", interrupted))
	}

	service := services{
		authn:         authn.NewService(users, sessions, tokens),
		users:         authn.NewUsersService(users, links),
		groups:        authn.NewGroupsService(groups),
		access:        access.NewService(checker, opened),
		spaces:        spaces.NewService(spaceList),
		audit:         audit.NewService(recorder),
		local:         NewLocalService(opened, users, links, s.config.PublicURL),
		jobs:          jobs.NewJobService(jobRunner),
		schedules:     jobs.NewSchedulesService(schedules),
		events:        events,
		topics:        topics,
		notifications: notifications,
	}

	build := edge.New(edge.Deps{
		Logger:       s.log,
		PublicURL:    s.config.PublicURL,
		CookieSecure: s.config.CookieSecure,
		Store:        opened,
		Users:        users,
		Sessions:     sessions,
		Tokens:       tokens,
		Links:        links,
		Groups:       groups,
		OIDC:         provider,
		OIDCAdmins:   s.config.Admins,
		Interceptors: interceptors,
		RegisterServices: func(mux *http.ServeMux, options []connect.HandlerOption) {
			service.register(mux, options)
		},
		RegisterLocal: func(mux *http.ServeMux, options []connect.HandlerOption) {
			path, handler := rookeryv1connect.NewLocalServiceHandler(service.local, options...)
			mux.Handle(path, handler)
		},
	})

	return &handlers{
		edge:      build.Handler(),
		socket:    build.SocketHandler(),
		log:       s.log,
		runner:    runner,
		scheduler: scheduler,
		retention: time.Duration(s.config.RetentionDays) * 24 * time.Hour,
		store:     opened,
	}, nil
}

// start runs what the leader runs besides its listeners: the job runner, the
// scheduler, and the loop that deletes records older than the retention. It
// returns when ctx ends.
func (h *handlers) start(ctx context.Context) error {
	group, ctx := errgroup.WithContext(ctx)
	group.Go(func() error { return h.runner.Run(ctx) })
	group.Go(func() error { return h.scheduler.Run(ctx) })
	group.Go(func() error {
		ticker := time.NewTicker(retentionCheck)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return nil
			case <-ticker.C:
				deleted, err := jobs.DeleteOlder(ctx, h.store, h.retention)
				if err != nil && ctx.Err() == nil {
					h.logger().Error("deleting old job records", slog.String("error", err.Error()))
				} else if deleted > 0 {
					h.logger().Info("deleted old job records", slog.Int64("jobs", deleted))
				}
			}
		}
	})
	return group.Wait()
}

// services are the edge API's handlers, wired once per server.
type services struct {
	authn         *authn.Service
	users         *authn.UsersService
	groups        *authn.GroupsService
	access        *access.Service
	spaces        *spaces.Service
	audit         *audit.Service
	local         *LocalService
	jobs          *jobs.JobService
	schedules     *jobs.SchedulesService
	events        *messaging.EventsService
	topics        *messaging.TopicsService
	notifications *messaging.NotificationsService
}

// register adds the core services to a mux.
func (s services) register(mux *http.ServeMux, options []connect.HandlerOption) {
	handle := func(path string, handler http.Handler) { mux.Handle(path, handler) }

	path, handler := rookeryv1connect.NewAuthServiceHandler(s.authn, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewUserServiceHandler(s.users, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewGroupServiceHandler(s.groups, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewAccessServiceHandler(s.access, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewSpaceServiceHandler(s.spaces, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewAuditServiceHandler(s.audit, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewJobServiceHandler(s.jobs, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewScheduleServiceHandler(s.schedules, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewEventServiceHandler(s.events, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewTopicServiceHandler(s.topics, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewNotificationServiceHandler(s.notifications, options...)
	handle(path, handler)
}

// LocalService is what the server's own machine may do without signing in. It is
// served on the Unix socket only.
type LocalService struct {
	store     *store.Store
	users     *authn.Users
	links     *authn.Links
	publicURL string
}

// NewLocalService returns LocalService for a database.
func NewLocalService(opened *store.Store, users *authn.Users, links *authn.Links, publicURL string) *LocalService {
	return &LocalService{store: opened, users: users, links: links, publicURL: publicURL}
}

// SetupLink makes the first admin's one-time link, and only while there is no
// admin.
func (l *LocalService) SetupLink(ctx context.Context, _ *connect.Request[rookeryv1.LocalServiceSetupLinkRequest]) (*connect.Response[rookeryv1.LocalServiceSetupLinkResponse], error) {
	admins, err := l.store.Queries().CountAdmins(ctx)
	if err != nil {
		return nil, access.ToConnect(fmt.Errorf("server: counting admins: %w", err))
	}
	if admins > 0 {
		return connect.NewResponse(&rookeryv1.LocalServiceSetupLinkResponse{Created: false, Url: ""}), nil
	}

	_, token, err := l.links.Create(ctx, "", "")
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.LocalServiceSetupLinkResponse{
		Url:     l.links.URL(token),
		Created: true,
	}), nil
}

// CreateToken makes an access token for a user, for a machine with no sign-in.
func (l *LocalService) CreateToken(ctx context.Context, request *connect.Request[rookeryv1.LocalServiceCreateTokenRequest]) (*connect.Response[rookeryv1.LocalServiceCreateTokenResponse], error) {
	user, err := l.users.ByEmail(ctx, request.Msg.GetEmail())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	tokens := authn.NewTokens(l.store)
	secret, _, err := tokens.Create(ctx, user.ID, request.Msg.GetName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.LocalServiceCreateTokenResponse{Token: secret}), nil
}

// Rekey reseals every sealed value under the current key. Secrets arrive with
// the settings, secrets and connections item; until then there is nothing to
// reseal and this says so.
func (l *LocalService) Rekey(context.Context, *connect.Request[rookeryv1.LocalServiceRekeyRequest]) (*connect.Response[rookeryv1.LocalServiceRekeyResponse], error) {
	return nil, connect.NewError(connect.CodeUnimplemented, errors.New("server: rekey arrives with secrets"))
}

// Reset drops and recreates the database's schemas.
func (l *LocalService) Reset(ctx context.Context, _ *connect.Request[rookeryv1.LocalServiceResetRequest]) (*connect.Response[rookeryv1.LocalServiceResetResponse], error) {
	if err := l.store.Reset(ctx); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.LocalServiceResetResponse{}), nil
}

// Dial opens a byte stream through the tailnet, for Ansible's SSH. The tailnet
// arrives with its own item.
func (l *LocalService) Dial(context.Context, *connect.BidiStream[rookeryv1.LocalServiceDialRequest, rookeryv1.LocalServiceDialResponse]) error {
	return connect.NewError(connect.CodeUnimplemented, errors.New("server: dial arrives with the tailnet"))
}
