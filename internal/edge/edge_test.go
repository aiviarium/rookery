package edge_test

import (
	"context"
	"io"
	"log/slog"
	"net/http"
	"net/http/cookiejar"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/audit"
	"github.com/aiviarium/rookery/internal/authn"
	"github.com/aiviarium/rookery/internal/edge"
	"github.com/aiviarium/rookery/internal/server"
	"github.com/aiviarium/rookery/internal/spaces"
	"github.com/aiviarium/rookery/internal/store"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// site is a test's edge: its two handlers and the pieces behind them.
type site struct {
	edge   *httptest.Server
	socket *httptest.Server

	users    *authn.Users
	sessions *authn.Sessions
	tokens   *authn.Tokens
	links    *authn.Links
	spaces   rookeryv1connect.SpaceServiceClient
}

// start serves an edge for a test database.
func start(t *testing.T) (*store.Store, *site) {
	t.Helper()

	opened := storetest.New(t)
	checker := access.NewChecker(opened)
	recorder := audit.NewRecorder(opened)
	users := authn.NewUsers(opened)
	sessions := authn.NewSessions(opened)
	tokens := authn.NewTokens(opened)
	links := authn.NewLinks(opened, "https://rookery.example.test")
	groups := authn.NewGroups(opened)
	spaceService := spaces.NewService(spaces.New(opened, checker))
	localService := server.NewLocalService(opened, users, links, "https://rookery.example.test")

	built := edge.New(edge.Deps{
		Logger:       testLogger(),
		PublicURL:    "https://rookery.example.test",
		CookieSecure: false,
		Store:        opened,
		Users:        users,
		Sessions:     sessions,
		Tokens:       tokens,
		Links:        links,
		Groups:       groups,
		Interceptors: []connect.Interceptor{
			audit.NewInterceptor(recorder, testLogger()),
			access.NewInterceptor(checker),
		},
		RegisterServices: func(mux *http.ServeMux, options []connect.HandlerOption) {
			path, handler := rookeryv1connect.NewSpaceServiceHandler(spaceService, options...)
			mux.Handle(path, handler)
			path, handler = rookeryv1connect.NewAuthServiceHandler(authn.NewService(users, sessions, tokens), options...)
			mux.Handle(path, handler)
		},
		RegisterLocal: func(mux *http.ServeMux, options []connect.HandlerOption) {
			path, handler := rookeryv1connect.NewLocalServiceHandler(localService, options...)
			mux.Handle(path, handler)
		},
	})

	edgeServer := httptest.NewServer(built.Handler())
	t.Cleanup(edgeServer.Close)
	socketServer := httptest.NewServer(built.SocketHandler())
	t.Cleanup(socketServer.Close)

	return opened, &site{
		edge:     edgeServer,
		socket:   socketServer,
		users:    users,
		sessions: sessions,
		tokens:   tokens,
		links:    links,
		spaces:   rookeryv1connect.NewSpaceServiceClient(edgeServer.Client(), edgeServer.URL, connect.WithGRPC()),
	}
}

// browser is an HTTP client with a cookie jar, as a browser is.
func browser(t *testing.T) *http.Client {
	t.Helper()

	jar, err := cookiejar.New(nil)
	if err != nil {
		t.Fatalf("making a cookie jar: %v", err)
	}
	return &http.Client{Jar: jar}
}

// get reads a page, and fails on anything but 200.
func get(t *testing.T, client *http.Client, address string) string {
	t.Helper()

	request, err := http.NewRequestWithContext(t.Context(), http.MethodGet, address, nil)
	if err != nil {
		t.Fatalf("building the call to %s: %v", address, err)
	}
	response, err := client.Do(request)
	if err != nil {
		t.Fatalf("getting %s: %v", address, err)
	}
	defer func() { _ = response.Body.Close() }()
	body, err := io.ReadAll(io.LimitReader(response.Body, 1<<20))
	if err != nil {
		t.Fatalf("reading %s: %v", address, err)
	}
	if response.StatusCode != http.StatusOK {
		t.Fatalf("%s answered %d: %s", address, response.StatusCode, body)
	}
	return string(body)
}

// cookies is a browser's jar as a Cookie header.
func cookies(t *testing.T, client *http.Client, address string) string {
	t.Helper()

	parsed, err := url.Parse(address)
	if err != nil {
		t.Fatalf("parsing %s: %v", address, err)
	}
	var parts []string
	for _, cookie := range client.Jar.Cookies(parsed) {
		parts = append(parts, cookie.Name+"="+cookie.Value)
	}
	return strings.Join(parts, "; ")
}

// csrf is the CSRF token a browser's jar holds.
func csrf(t *testing.T, client *http.Client, address string) string {
	t.Helper()

	parsed, err := url.Parse(address)
	if err != nil {
		t.Fatalf("parsing %s: %v", address, err)
	}
	for _, cookie := range client.Jar.Cookies(parsed) {
		if cookie.Name == "rookery_csrf" {
			return cookie.Value
		}
	}
	t.Fatal("the browser has no CSRF cookie")
	return ""
}

// call makes a call with a CSRF header, as the client half does. The first
// argument is a generated client's method, such as me.Me.
func call[Req, Res any](t *testing.T, method func(context.Context, *connect.Request[Req]) (*connect.Response[Res], error), request *Req, cookieHeader, csrfToken string) (*Res, error) {
	t.Helper()

	built := connect.NewRequest(request)
	if cookieHeader != "" {
		built.Header().Set("Cookie", cookieHeader)
	}
	if csrfToken != "" {
		built.Header().Set(edge.CSRFHeader, csrfToken)
	}
	response, err := method(t.Context(), built)
	if err != nil {
		return nil, err
	}
	return response.Msg, nil
}

func TestTheFirstLinkSetsUpAnAdmin(t *testing.T) {
	t.Parallel()

	opened, site := start(t)
	ctx := t.Context()

	// The server's own machine makes the setup link, over the socket.
	socket := rookeryv1connect.NewLocalServiceClient(site.socket.Client(), site.socket.URL, connect.WithGRPC())
	made, err := socket.SetupLink(ctx, connect.NewRequest(&rookeryv1.LocalServiceSetupLinkRequest{}))
	if err != nil {
		t.Fatalf("making the setup link: %v", err)
	}
	if !made.Msg.GetCreated() {
		t.Fatal("an empty deployment made no setup link")
	}
	linkURL := made.Msg.GetUrl()
	if !strings.HasSuffix(linkURL, "/auth/link/"+lastSegment(linkURL)) {
		t.Fatalf("the link is %q", linkURL)
	}

	client := browser(t)
	page := get(t, client, site.edge.URL+"/auth/link/"+lastSegment(linkURL))
	if !strings.Contains(page, `name="email"`) {
		t.Fatalf("the setup link showed no form: %s", page)
	}

	// The form makes the admin, and the browser comes back signed in.
	form := url.Values{
		"email": {"first@example.test"},
		"name":  {"First Admin"},
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, site.edge.URL+"/auth/link/"+lastSegment(linkURL), strings.NewReader(form.Encode()))
	if err != nil {
		t.Fatalf("building the form's call: %v", err)
	}
	request.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	response, err := client.Do(request)
	if err != nil {
		t.Fatalf("posting the form: %v", err)
	}
	defer func() { _ = response.Body.Close() }()
	if response.StatusCode != http.StatusOK {
		t.Fatalf("the form answered %d", response.StatusCode)
	}

	admin, err := site.users.ByEmail(ctx, "first@example.test")
	if err != nil {
		t.Fatalf("the admin was not made: %v", err)
	}
	if !admin.Admin {
		t.Fatal("the first user is not an admin")
	}
	if admins, err := opened.Queries().CountAdmins(ctx); err != nil || admins != 1 {
		t.Fatalf("the deployment has %d admins (%v)", admins, err)
	}

	// A second setup link is refused, because there is an admin.
	again, err := socket.SetupLink(ctx, connect.NewRequest(&rookeryv1.LocalServiceSetupLinkRequest{}))
	if err != nil {
		t.Fatalf("asking for a second setup link: %v", err)
	}
	if again.Msg.GetCreated() || again.Msg.GetUrl() != "" {
		t.Fatal("a deployment with an admin made another setup link")
	}

	// The browser's cookie makes calls, with the CSRF header.
	me := rookeryv1connect.NewAuthServiceClient(site.edge.Client(), site.edge.URL, connect.WithGRPC())
	who, err := call(t, me.Me, &rookeryv1.AuthServiceMeRequest{}, cookies(t, client, site.edge.URL), csrf(t, client, site.edge.URL))
	if err != nil {
		t.Fatalf("Me with the cookie: %v", err)
	}
	if who.GetUser().GetEmail() != "first@example.test" {
		t.Fatalf("the cookie is %q's", who.GetUser().GetEmail())
	}

	// Without the CSRF header the same call is refused.
	if _, err := call(t, me.Me, &rookeryv1.AuthServiceMeRequest{}, cookies(t, client, site.edge.URL), ""); err == nil {
		t.Fatal("a cookie's call without its CSRF header went through")
	} else if connect.CodeOf(err) != connect.CodePermissionDenied {
		t.Fatalf("a call without the CSRF header failed with %v", err)
	}

	// And the cookie makes a state-changing call, which the audit keeps.
	spaceCall := connect.NewRequest(&rookeryv1.SpaceServiceCreateRequest{Kind: "general", Name: "Team"})
	spaceCall.Header().Set("Cookie", cookies(t, client, site.edge.URL))
	spaceCall.Header().Set(edge.CSRFHeader, csrf(t, client, site.edge.URL))
	created, err := rookeryv1connect.NewSpaceServiceClient(site.edge.Client(), site.edge.URL, connect.WithGRPC()).Create(ctx, spaceCall)
	if err != nil {
		t.Fatalf("creating a space with the cookie: %v", err)
	}
	if created.Msg.GetSpace().GetName() != "Team" {
		t.Fatalf("the space is %q", created.Msg.GetSpace().GetName())
	}
}

func TestALinkForAnEmailSignsThatPersonIn(t *testing.T) {
	t.Parallel()

	_, site := start(t)
	ctx := t.Context()

	admin, err := site.users.Ensure(ctx, "admin@example.test", "Admin", true)
	if err != nil {
		t.Fatalf("making the admin: %v", err)
	}
	link, token, err := site.links.Create(ctx, "new@example.test", admin.ID)
	if err != nil {
		t.Fatalf("making the link: %v", err)
	}
	_ = link

	client := browser(t)
	page := get(t, client, site.edge.URL+"/auth/link/"+token)
	if strings.Contains(page, `name="email"`) {
		t.Fatal("a link for an email showed the setup form")
	}

	// The user was made by the link, and the cookie is theirs.
	user, err := site.users.ByEmail(ctx, "new@example.test")
	if err != nil {
		t.Fatalf("the link made no user: %v", err)
	}
	me := rookeryv1connect.NewAuthServiceClient(site.edge.Client(), site.edge.URL, connect.WithGRPC())
	who, err := call(t, me.Me, &rookeryv1.AuthServiceMeRequest{}, cookies(t, client, site.edge.URL), csrf(t, client, site.edge.URL))
	if err != nil {
		t.Fatalf("Me with the cookie: %v", err)
	}
	if who.GetUser().GetId() != user.ID {
		t.Fatalf("the cookie is %q's, want %q", who.GetUser().GetId(), user.ID)
	}

	// The link works once.
	spent, err := http.NewRequestWithContext(ctx, http.MethodGet, site.edge.URL+"/auth/link/"+token, nil)
	if err != nil {
		t.Fatalf("building the second call: %v", err)
	}
	again, err := client.Do(spent)
	if err != nil {
		t.Fatalf("using the link again: %v", err)
	}
	defer func() { _ = again.Body.Close() }()
	if again.StatusCode != http.StatusNotFound {
		t.Fatalf("a spent link answered %d", again.StatusCode)
	}
}

func TestAnAccessTokenSignsACommandIn(t *testing.T) {
	t.Parallel()

	_, site := start(t)
	ctx := t.Context()

	user, err := site.users.Ensure(ctx, "script@example.test", "Script", false)
	if err != nil {
		t.Fatalf("making the user: %v", err)
	}
	secret, _, err := site.tokens.Create(ctx, user.ID, "a script")
	if err != nil {
		t.Fatalf("making the token: %v", err)
	}

	connection, err := dialWithToken(site.edge.URL, secret)
	if err != nil {
		t.Fatalf("dialing: %v", err)
	}
	me, err := connection.Auth.Me(ctx, connect.NewRequest(&rookeryv1.AuthServiceMeRequest{}))
	if err != nil {
		t.Fatalf("Me with a token: %v", err)
	}
	if me.Msg.GetUser().GetEmail() != "script@example.test" {
		t.Fatalf("the token is %q's", me.Msg.GetUser().GetEmail())
	}
}

func TestTheLocalServiceIsNotOnTheEdge(t *testing.T) {
	t.Parallel()

	_, site := start(t)
	ctx := t.Context()

	// Signed in as an admin, LocalService is still not this caller's: it is the
	// Unix socket's.
	admin, err := site.users.Ensure(ctx, "admin@example.test", "Admin", true)
	if err != nil {
		t.Fatalf("making the admin: %v", err)
	}
	secret, _, err := site.tokens.Create(ctx, admin.ID, "a script")
	if err != nil {
		t.Fatalf("making the token: %v", err)
	}
	connection, err := dialWithToken(site.edge.URL, secret)
	if err != nil {
		t.Fatalf("dialing: %v", err)
	}

	if _, err := connection.Local.SetupLink(ctx, connect.NewRequest(&rookeryv1.LocalServiceSetupLinkRequest{})); err == nil {
		t.Fatal("an admin made a setup link through the edge")
	} else if connect.CodeOf(err) != connect.CodeUnimplemented {
		t.Fatalf("LocalService on the edge failed with %v, want the route to be absent", err)
	}
}

func TestLogoutEndsTheSession(t *testing.T) {
	t.Parallel()

	_, site := start(t)
	ctx := t.Context()

	if _, err := site.users.Ensure(ctx, "person@example.test", "Person", false); err != nil {
		t.Fatalf("making the user: %v", err)
	}
	_, token, err := site.links.Create(ctx, "person@example.test", "")
	if err != nil {
		t.Fatalf("making the link: %v", err)
	}

	client := browser(t)
	get(t, client, site.edge.URL+"/auth/link/"+token)

	request, err := http.NewRequestWithContext(ctx, http.MethodPost, site.edge.URL+"/auth/logout", nil)
	if err != nil {
		t.Fatalf("building the logout: %v", err)
	}
	request.Header.Set(edge.CSRFHeader, csrf(t, client, site.edge.URL))
	response, err := client.Do(request)
	if err != nil {
		t.Fatalf("logging out: %v", err)
	}
	defer func() { _ = response.Body.Close() }()
	if response.StatusCode != http.StatusOK {
		t.Fatalf("logout answered %d", response.StatusCode)
	}

	sessions, err := site.sessions.List(ctx, mustUser(t, site.users, "person@example.test").ID)
	if err != nil {
		t.Fatalf("listing sessions: %v", err)
	}
	if len(sessions) != 0 {
		t.Fatalf("logout left %d sessions", len(sessions))
	}
}

// lastSegment is the last part of a URL's path.
func lastSegment(address string) string {
	parts := strings.Split(strings.TrimRight(address, "/"), "/")
	return parts[len(parts)-1]
}

// mustUser is a user by email, or a failed test.
func mustUser(t *testing.T, users *authn.Users, email string) store.RookeryUsers {
	t.Helper()

	user, err := users.ByEmail(t.Context(), email)
	if err != nil {
		t.Fatalf("reading %s: %v", email, err)
	}
	return user
}

// testLogger is a logger nothing hears.
func testLogger() *slog.Logger { return slog.New(slog.DiscardHandler) }

// clients are the two services a token's call needs.
type clients struct {
	Auth  rookeryv1connect.AuthServiceClient
	Local rookeryv1connect.LocalServiceClient
}

// dialWithToken is a connection that carries an access token, as the CLI does.
func dialWithToken(address, token string) (*clients, error) {
	client := &http.Client{Transport: tokenTransport{next: http.DefaultTransport, token: token}}
	return &clients{
		Auth:  rookeryv1connect.NewAuthServiceClient(client, address, connect.WithGRPC()),
		Local: rookeryv1connect.NewLocalServiceClient(client, address, connect.WithGRPC()),
	}, nil
}

// tokenTransport adds the bearer token to every call.
type tokenTransport struct {
	next  http.RoundTripper
	token string
}

// RoundTrip adds the Authorization header.
func (t tokenTransport) RoundTrip(request *http.Request) (*http.Response, error) {
	clone := request.Clone(request.Context())
	clone.Header.Set("Authorization", "Bearer "+t.token)
	return t.next.RoundTrip(clone)
}
