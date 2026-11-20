// Package edge is the server's public listener: the core services as gRPC,
// Connect and gRPC-Web, the sign-in routes a browser and the CLI use, and the
// authentication that turns a cookie or a token into a principal.
//
// The same services are served on the server's Unix socket, as its local admin,
// and `LocalService` is served nowhere else.
package edge

import (
	"context"
	"log/slog"
	"net/http"
	"strings"
	"time"

	"connectrpc.com/connect"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/authn"
	"github.com/aiviarium/rookery/internal/store"
)

// Deps is what the edge is built from.
type Deps struct {
	Logger       *slog.Logger
	PublicURL    string
	CookieSecure bool

	Store    *store.Store
	Users    *authn.Users
	Sessions *authn.Sessions
	Tokens   *authn.Tokens
	Links    *authn.Links
	Groups   *authn.Groups
	// OIDC is nil when the deployment has no identity provider.
	OIDC *authn.OIDC
	// OIDCAdmins are the emails that start as admins.
	OIDCAdmins []string

	// Interceptors are the access and audit interceptors, outermost first.
	Interceptors []connect.Interceptor

	// Plugins serves the client halves and the shell of the installed plugins,
	// when the servers has any.
	Plugins Plugins
	// WebShell is the shell Rookery ships, served for `/` and unmatched paths
	// until an installed plugin is the shell.
	WebShell http.Handler
	// Desktop serves the RFB WebSocket when it is set.
	Desktop http.Handler
	// Blobs serves /blobs/{id} when it is set.
	Blobs http.Handler
	// Documents serves /ws/documents/{id} when configured.
	Documents http.Handler
	// Webhooks authenticate with the secret declared by each plugin.
	Webhooks http.Handler
	// Services starts a plugin's service for a call. Without it the edge serves
	// client halves only.
	Services PluginServices
	// Procedures is what the installed plugins declare their procedures need.
	Procedures PluginProcedures
	// PluginTokens is the plugin whose service holds a token, for the socket.
	PluginTokens PluginTokens
	// Callers issues and verifies credentials delegated by browser RPCs.
	Callers *access.CallerTokens

	// RegisterServices adds the core services to a mux, both listeners.
	RegisterServices func(mux *http.ServeMux, options []connect.HandlerOption)
	// RegisterLocal adds LocalService, the Unix socket only.
	RegisterLocal func(mux *http.ServeMux, options []connect.HandlerOption)
	// RegisterHostRoutes adds the public terminal and port endpoints.
	RegisterHostRoutes func(mux *http.ServeMux)
}

// Edge is the edge's handlers.
type Edge struct {
	deps Deps

	codes *codeStore
}

// New builds the edge's handlers.
func New(deps Deps) *Edge {
	return &Edge{deps: deps, codes: newCodeStore(5 * time.Minute)}
}

// Handler is the public listener's handler: the sign-in routes, every core
// service, and the shell's fallback.
func (e *Edge) Handler() http.Handler {
	mux := http.NewServeMux()
	e.deps.RegisterServices(mux, e.handlerOptions())
	if e.deps.Blobs != nil {
		mux.Handle("/blobs/", e.deps.Blobs)
	}
	if e.deps.Documents != nil {
		mux.Handle("/ws/documents/", e.deps.Documents)
	}
	if e.deps.Desktop != nil {
		mux.Handle("GET /ws/hosts/{name}/desktop", e.deps.Desktop)
	}
	e.registerAuth(mux)
	e.registerPlugins(mux)
	e.registerPluginRPC(mux)
	if e.deps.RegisterHostRoutes != nil {
		e.deps.RegisterHostRoutes(mux)
	}
	mux.HandleFunc("/", e.fallback)
	if e.deps.Webhooks == nil {
		return e.authenticate(mux)
	}
	public := http.NewServeMux()
	public.Handle("POST /hooks/{plugin}/{name}", e.deps.Webhooks)
	public.HandleFunc("/hooks/{plugin}/{name}", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Allow", http.MethodPost)
		http.Error(w, "Method not allowed.", http.StatusMethodNotAllowed)
	})
	public.Handle("/", e.authenticate(mux))
	return public
}

// SocketHandler is the Unix socket's handler: every core service as the local
// admin, and LocalService.
func (e *Edge) SocketHandler() http.Handler {
	mux := http.NewServeMux()
	e.deps.RegisterServices(mux, e.handlerOptions())
	e.deps.RegisterLocal(mux, e.handlerOptions())
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// The socket is the local admin's, and a plugin's service reaches it
		// with the token the server started it with. A token that is not one of
		// those is not a lesser admin: it is refused.
		ctx := r.Context()
		authorization := r.Header.Get("Authorization")
		caller := r.Header.Get(access.CallerTokenHeader)
		if authorization == "" && caller == "" {
			ctx = access.WithPrincipal(ctx, access.LocalAdmin())
			if space := r.Header.Get("Rookery-Space"); space != "" {
				ctx = access.WithSpace(ctx, space)
			}
		} else {
			secret, bearer := strings.CutPrefix(authorization, "Bearer ")
			if !bearer || e.deps.PluginTokens == nil {
				unauthorized(w)
				return
			}
			secret = strings.TrimSpace(secret)
			plugin, ok := e.deps.PluginTokens.PluginByToken(secret)
			if !ok {
				unauthorized(w)
				return
			}
			if tokens, ok := e.deps.PluginTokens.(interface{ ProvisionerNode(string) (string, bool) }); ok {
				if node, provisioner := tokens.ProvisionerNode(secret); provisioner {
					ctx = access.WithProvisionerNode(ctx, node)
				}
			}
			principal := access.Plugin(plugin)
			space := r.Header.Get("Rookery-Space")
			if caller != "" {
				if e.deps.Callers == nil {
					unauthorized(w)
					return
				}
				userID, currentSpace, err := e.deps.Callers.Verify(plugin, caller)
				if err != nil {
					unauthorized(w)
					return
				}
				user, err := e.deps.Users.ByID(ctx, userID)
				if err != nil || user.Disabled {
					unauthorized(w)
					return
				}
				principal, space = authn.Principal(user), currentSpace
			}
			ctx = access.WithPluginIdentity(access.WithPrincipal(ctx, principal), plugin)
			if space != "" {
				ctx = access.WithSpace(ctx, space)
			}
		}
		mux.ServeHTTP(w, r.WithContext(ctx))
	})
}

// handlerOptions are the connect options every handler gets: the audit and
// access interceptors, and the CSRF check for a cookie's calls.
func (e *Edge) handlerOptions() []connect.HandlerOption {
	interceptors := make([]connect.Interceptor, 0, len(e.deps.Interceptors)+1)
	interceptors = append(interceptors, e.deps.Interceptors...)
	interceptors = append(interceptors, csrfInterceptor{})
	return []connect.HandlerOption{connect.WithInterceptors(interceptors...)}
}

// authenticate resolves the caller from a token or a session cookie, and puts the
// current space and the session in the context.
func (e *Edge) authenticate(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		ctx := r.Context()
		if space := r.Header.Get("Rookery-Space"); space != "" {
			ctx = access.WithSpace(ctx, space)
		}

		principal, session, found, err := e.principalOf(r)
		switch {
		case err != nil:
			unauthorized(w)
			return
		case found:
			user, userErr := e.deps.Users.ByID(ctx, principal.ID)
			if userErr != nil {
				unauthorized(w)
				return
			}
			if user.Disabled {
				unauthorized(w)
				return
			}
			ctx = access.WithPrincipal(ctx, principal)
			if session.ID != "" {
				ctx = authn.WithSessionRecord(ctx, session)
			}
		}
		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

// principalOf is the caller a request names: a bearer token, or the session
// cookie. It reports whether there was one at all.
func (e *Edge) principalOf(r *http.Request) (access.Principal, store.RookerySessions, bool, error) {
	ctx := r.Context()

	if header := r.Header.Get("Authorization"); strings.HasPrefix(header, "Bearer ") {
		secret := strings.TrimSpace(strings.TrimPrefix(header, "Bearer "))
		if secret == "" {
			return access.Principal{}, store.RookerySessions{}, false, nil
		}
		token, err := e.deps.Tokens.Lookup(ctx, secret)
		if err != nil {
			return access.Principal{}, store.RookerySessions{}, false, err
		}
		user, err := e.deps.Users.ByID(ctx, token.UserID)
		if err != nil {
			return access.Principal{}, store.RookerySessions{}, false, err
		}
		return authn.Principal(user), store.RookerySessions{}, true, nil
	}

	cookie, err := r.Cookie(sessionCookie)
	if err != nil || cookie.Value == "" {
		return access.Principal{}, store.RookerySessions{}, false, nil
	}
	session, err := e.deps.Sessions.FromSecret(ctx, cookie.Value)
	if err != nil {
		return access.Principal{}, store.RookerySessions{}, false, err
	}
	user, err := e.deps.Users.ByID(ctx, session.UserID)
	if err != nil {
		return access.Principal{}, store.RookerySessions{}, false, err
	}
	return authn.Principal(user), session, true, nil
}

// csrfInterceptor refuses a call that a session cookie authenticated without the
// session's CSRF token in the header. A token's call has no session and needs
// none: a token is not a cookie a browser sends by itself.
type csrfInterceptor struct{}

// WrapUnary checks a unary call.
func (csrfInterceptor) WrapUnary(next connect.UnaryFunc) connect.UnaryFunc {
	return func(ctx context.Context, request connect.AnyRequest) (connect.AnyResponse, error) {
		if err := checkCSRF(ctx, request.Header()); err != nil {
			return nil, err
		}
		return next(ctx, request)
	}
}

// WrapStreamingHandler checks a streaming call.
func (csrfInterceptor) WrapStreamingHandler(next connect.StreamingHandlerFunc) connect.StreamingHandlerFunc {
	return func(ctx context.Context, conn connect.StreamingHandlerConn) error {
		if err := checkCSRF(ctx, conn.RequestHeader()); err != nil {
			return err
		}
		return next(ctx, conn)
	}
}

// WrapStreamingClient passes a call through: this side is a client.
func (csrfInterceptor) WrapStreamingClient(next connect.StreamingClientFunc) connect.StreamingClientFunc {
	return next
}

// CSRFHeader is the header a browser's call echoes its CSRF token in.
const CSRFHeader = "Rookery-CSRF"

// checkCSRF refuses a cookie-authenticated call without the CSRF token.
func checkCSRF(ctx context.Context, header http.Header) error {
	session, ok := authn.SessionRecordFrom(ctx)
	if !ok || session.Csrf == "" {
		return nil
	}
	if header.Get(CSRFHeader) != session.Csrf {
		return connect.NewError(connect.CodePermissionDenied, access.ErrDenied)
	}
	return nil
}

// unauthorized answers a request whose credentials are not good.
func unauthorized(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	w.WriteHeader(http.StatusUnauthorized)
	_, _ = w.Write([]byte("not signed in\n"))
}
