package server

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"path/filepath"
	"time"

	"golang.org/x/sync/errgroup"

	"connectrpc.com/connect"

	pluginv1connect "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1/pluginv1connect"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/audit"
	"github.com/aiviarium/rookery/internal/authn"
	"github.com/aiviarium/rookery/internal/blobs"
	"github.com/aiviarium/rookery/internal/config"
	"github.com/aiviarium/rookery/internal/desktop"
	"github.com/aiviarium/rookery/internal/docs"
	"github.com/aiviarium/rookery/internal/edge"
	"github.com/aiviarium/rookery/internal/hosts"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/link"
	"github.com/aiviarium/rookery/internal/messaging"
	"github.com/aiviarium/rookery/internal/plugins"
	"github.com/aiviarium/rookery/internal/rollouts"
	"github.com/aiviarium/rookery/internal/seal"
	"github.com/aiviarium/rookery/internal/spaces"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/streamio"
)

// wire builds every service and both listeners' handlers. Every dependency goes
// in here, in one place, in the order start-up needs it.
func (s *Server) wire(ctx context.Context, opened *store.Store) (*handlers, error) {
	key, err := seal.Load(s.config.SecretKey, s.config.SecretKeyFile, true)
	if err != nil {
		return nil, err
	}
	ring, err := seal.NewRing(key.ID(), key)
	if err != nil {
		return nil, err
	}
	s.log.Debug("the sealing key is loaded", slog.String("key_id", key.ID()))

	checker := access.NewChecker(opened)
	recorder := audit.NewRecorder(opened)

	users := authn.NewUsers(opened)
	sessions := authn.NewSessions(opened)
	tokens := authn.NewTokens(opened)
	links := authn.NewLinks(opened, s.config.PublicURL)
	groups := authn.NewGroups(opened)
	spaceList := spaces.New(opened, checker)
	spaceKinds := spaces.NewRegistry()

	blobLocation := s.config.Blobs
	if blobLocation == "" {
		blobLocation = filepath.Join(s.config.StateDir, "blobs")
	}
	backend, err := blobs.NewLocation(blobLocation, blobs.S3Config{
		Endpoint:     s.config.S3Endpoint,
		AccessKey:    s.config.S3AccessKey,
		SecretKey:    s.config.S3SecretKey,
		Region:       s.config.S3Region,
		Secure:       s.config.S3Secure,
		UsePathStyle: s.config.S3PathStyle,
	})
	if err != nil {
		return nil, err
	}
	blobStore := blobs.New(opened, backend, checker)
	settings := config.NewSettings(opened)
	secrets := config.NewSecrets(opened, ring)
	connections := config.NewConnections(opened, ring)

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
	topics := messaging.NewTopicsService(messaging.NewTopics(opened), checker)
	notificationStore := messaging.NewNotifications(opened)
	notifications := messaging.NewNotificationsService(notificationStore)

	registry := jobs.NewRegistry()
	if err := registry.RegisterKinds(jobs.CoreKinds()...); err != nil {
		return nil, err
	}
	runner := jobs.NewRunner(opened, registry, events, s.log)
	runner.OnFailure(notifyJobFailure(notificationStore, s.log))
	jobRunner := jobs.NewService(opened, checker, registry, runner, s.log)

	schedules := jobs.NewSchedules(opened, registry, jobRunner, s.log)

	collections, err := wireCollections(ctx, opened, checker, ring)
	if err != nil {
		return nil, err
	}

	// Plugins install their declared collections through the datastore.
	installer := plugins.NewInstaller(plugins.Options{
		Version:   s.config.Version,
		Store:     opened,
		Data:      collections.data,
		StateDir:  s.config.StateDir,
		Jobs:      registry,
		Schedules: schedules,
		Spaces:    spaceKinds,
		Logger:    s.log,
	})
	supervisor := plugins.NewSupervisor(plugins.SupervisorOptions{
		Source:   installer,
		StateDir: s.config.StateDir,
		Socket:   s.config.Socket,
		Logger:   s.log,
	})
	installer.SetServices(supervisor)

	hostStore := hosts.New(opened)
	rolloutList := rollouts.New(rollouts.Options{
		Store:         opened,
		Checker:       checker,
		Jobs:          jobRunner,
		Hosts:         rolloutTargets{hosts: hostStore, version: s.config.Version, versions: installedPluginVersions(installer)},
		Notifications: notificationStore,
		Publisher:     events,
		Logger:        s.log,
	})
	schedules.SetRolloutStarter(rolloutList)
	scheduler := jobs.NewScheduler(schedules)
	rolloutManager := rollouts.NewManager(rolloutList)

	// Hosts, their links and the key ring. A link is opened on demand; the
	// first one makes the active key on a database that has none.
	keyRing := link.NewRing(opened, ring)
	trusted := link.NewKnownHosts(opened, s.log)
	hostService := hosts.NewService(hosts.Deps{
		Hosts:    hostStore,
		Checker:  checker,
		Bus:      bus,
		Version:  s.config.Version,
		Log:      s.log,
		Versions: installedPluginVersions(installer),
	})
	hostLinks := link.NewManager(opened, keyRing, trusted, link.Options{
		Targets: hostService.Targets(),
		Log:     s.log,
	})

	hostService.SetJobs(jobRunner)
	hostService.SetLinks(hostLinks)
	hostRuntime := hosts.NewRuntime(hostService)
	if err := registry.Handle("host.contact", hostService.ContactHandler()); err != nil {
		return nil, err
	}

	if interrupted, err := jobs.InterruptRunning(ctx, opened); err != nil {
		return nil, err
	} else if interrupted > 0 {
		s.log.Info("interrupted the jobs a stopped server left running", slog.Int64("jobs", interrupted))
	}
	// A roll-out is never resumed by itself after a leader change.
	if paused, err := rollouts.PauseLeftRunning(ctx, opened); err != nil {
		return nil, err
	} else if paused > 0 {
		s.log.Info("paused the roll-outs a stopped server left running", slog.Int64("rollouts", paused))
	}

	provisioners, provisionTokens, err := wireProvisioners(opened, checker, secrets, installer, supervisor, s.config, s.log)
	if err != nil {
		return nil, err
	}
	if err := provisioners.RunActionsAsJobs(registry, jobRunner); err != nil {
		return nil, err
	}
	service := services{
		authn:         authn.NewService(users, sessions, tokens),
		users:         authn.NewUsersService(users, links),
		groups:        authn.NewGroupsService(groups),
		access:        access.NewService(checker, opened),
		spaces:        spaces.NewService(spaceList, spaceKinds),
		audit:         audit.NewService(recorder),
		local:         NewLocalService(opened, users, links, secrets, s.config.PublicURL),
		jobs:          jobs.NewJobService(jobRunner),
		schedules:     jobs.NewSchedulesService(schedules),
		events:        events,
		topics:        topics,
		notifications: notifications,
		plugins:       plugins.NewService(installer),
		registries:    plugins.NewRegistries(opened),
		settings:      config.NewSettingsService(settings, checker),
		secrets:       config.NewSecretsService(secrets),
		connections:   config.NewConnectionsService(connections),
		blobs:         blobs.NewService(blobStore),
		hosts:         hostService,
		desktop:       desktop.New(hostStore, hostLinks, checker),
		hostRuntime:   hostRuntime,
		keys:          link.NewKeysService(keyRing, trusted),
		rollouts:      rollouts.NewRolloutService(rolloutList),
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
		Plugins:      installer,
		Services:     supervisor,
		Procedures:   installer,
		PluginTokens: provisionTokens,
		RegisterServices: func(mux *http.ServeMux, options []connect.HandlerOption) {
			service.register(mux, options)
			collections.register(mux, options)
			path, handler := rookeryv1connect.NewProvisionerServiceHandler(provisioners, options...)
			mux.Handle(path, handler)
		},
		Blobs:              blobStore.Handler(),
		Documents:          docs.NewHandler(collections.docs, dataCaller{}),
		Desktop:            service.desktop.WebSocket(),
		RegisterHostRoutes: hostRuntime.RegisterRoutes,
		RegisterLocal: func(mux *http.ServeMux, options []connect.HandlerOption) {
			path, handler := rookeryv1connect.NewLocalServiceHandler(service.local, options...)
			mux.Handle(path, handler)
			path, handler = pluginv1connect.NewServerServiceHandler(NewPluginServer(bus, notificationStore), options...)
			mux.Handle(path, handler)
		},
	})

	return &handlers{
		edge:        build.Handler(),
		socket:      build.SocketHandler(),
		log:         s.log,
		runner:      runner,
		scheduler:   scheduler,
		supervisor:  supervisor,
		retention:   time.Duration(s.config.RetentionDays) * 24 * time.Hour,
		store:       opened,
		blobs:       blobStore,
		links:       hostLinks,
		rollouts:    rolloutManager,
		collections: collections,
		hostRuntime: hostRuntime,
	}, nil
}

// start runs what the leader runs besides its listeners: the job runner, the
// scheduler, and the loop that deletes records older than the retention. It
// returns when ctx ends.
func (h *handlers) start(ctx context.Context) error {
	group, ctx := errgroup.WithContext(ctx)
	h.supervisor.StartAll(ctx)
	defer h.supervisor.StopAll()
	group.Go(func() error { return h.runner.Run(ctx) })
	group.Go(func() error { return h.scheduler.Run(ctx) })
	group.Go(func() error { return h.rollouts.Run(ctx) })
	group.Go(func() error { return h.collections.data.Run(ctx) })
	group.Go(func() error { return h.collectExpiring(ctx) })
	group.Go(func() error { return h.hostRuntime.FollowChannels(ctx) })
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
	plugins       *plugins.PluginService
	registries    *plugins.Registries
	settings      *config.SettingsService
	secrets       *config.SecretsService
	connections   *config.ConnectionsService
	blobs         *blobs.Service
	hosts         *hosts.Service
	desktop       *desktop.Service
	hostRuntime   *hosts.Runtime
	keys          *link.KeysService
	rollouts      *rollouts.RolloutService
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
	path, handler = rookeryv1connect.NewPluginServiceHandler(s.plugins, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewRegistryServiceHandler(s.registries, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewRolloutServiceHandler(s.rollouts, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewSettingServiceHandler(s.settings, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewSecretServiceHandler(s.secrets, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewConnectionServiceHandler(s.connections, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewBlobServiceHandler(s.blobs, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewHostServiceHandler(s.hosts, options...)
	handle(path, handler)
	path, handler = rookeryv1connect.NewDesktopServiceHandler(s.desktop, options...)
	handle(path, streamio.WithReadInterrupt(handler))
	registerHostRuntime(mux, s.hostRuntime, options)
	path, handler = rookeryv1connect.NewKeyServiceHandler(s.keys, options...)
	handle(path, handler)
}

// installedPluginVersions is the versions of the installed plugins, which the
// applied digest is taken over: a plugin update makes every host whose profile
// names it behind.
func installedPluginVersions(installer *plugins.Installer) func(context.Context) (map[string]string, error) {
	return func(ctx context.Context) (map[string]string, error) {
		installed, err := installer.List(ctx)
		if err != nil {
			return nil, err
		}
		versions := make(map[string]string, len(installed))
		for _, plugin := range installed {
			versions[plugin.ID] = plugin.Version
		}
		return versions, nil
	}
}

// LocalService is what the server's own machine may do without signing in. It is
// served on the Unix socket only.
type LocalService struct {
	store     *store.Store
	users     *authn.Users
	links     *authn.Links
	secrets   *config.Secrets
	publicURL string
}

// NewLocalService returns LocalService for a database.
func NewLocalService(opened *store.Store, users *authn.Users, links *authn.Links, secrets *config.Secrets, publicURL string) *LocalService {
	return &LocalService{store: opened, users: users, links: links, secrets: secrets, publicURL: publicURL}
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

// Rekey reseals every sealed secret and every connection under the current key.
func (l *LocalService) Rekey(ctx context.Context, _ *connect.Request[rookeryv1.LocalServiceRekeyRequest]) (*connect.Response[rookeryv1.LocalServiceRekeyResponse], error) {
	if l.secrets == nil {
		return nil, connect.NewError(connect.CodeUnavailable, errors.New("server: no secrets are wired"))
	}
	rows, err := l.secrets.Rekey(ctx)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.LocalServiceRekeyResponse{Rows: rows}), nil
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
