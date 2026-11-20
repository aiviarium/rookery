package edge

import (
	"context"
	"crypto/subtle"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/authn"
	"github.com/aiviarium/rookery/internal/id"
	"github.com/aiviarium/rookery/internal/store"
)

// Cookie names. The session's secret and the CSRF token are separate cookies so
// that the client half can read the CSRF token: the session's is HttpOnly.
const (
	sessionCookie = "rookery_session"
	csrfCookie    = "rookery_csrf"
	oidcCookie    = "rookery_oidc"
)

// oidcCookieLifetime is how long a sign-in attempt may take.
const oidcCookieLifetime = 10 * time.Minute

// setupPage is the form the first admin fills in.
const setupPage = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Set up Rookery</title></head>
<body>
<h1>Set up Rookery</h1>
<p>This link makes the first admin.</p>
<form method="post">
<label>Email <input name="email" type="email" required></label>
<label>Name <input name="name" required></label>
<button type="submit">Create the admin</button>
</form>
</body></html>`

// cliPage is the page a browser shows when `rookery login` sent it here.
const cliPage = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Sign in the CLI</title></head>
<body>
<h1>Sign in the CLI</h1>
<p><code>rookery</code> on this machine asked to sign in as %s.</p>
<form method="post">
<input type="hidden" name="challenge" value="%s">
<input type="hidden" name="port" value="%s">
<input type="hidden" name="state" value="%s">
<button type="submit">Sign in the CLI</button>
</form>
</body></html>`

// fallbackPage is what `/` says until a shell plugin is installed.
const fallbackPage = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Rookery</title></head>
<body><h1>Rookery</h1><p>No shell plugin is installed.</p></body></html>`

// registerAuth adds the sign-in routes.
func (e *Edge) registerAuth(mux *http.ServeMux) {
	mux.HandleFunc("/auth/link/", e.link)
	mux.HandleFunc("/auth/oidc/start", e.oidcStart)
	mux.HandleFunc("/auth/oidc/callback", e.oidcCallback)
	mux.HandleFunc("/auth/cli", e.cli)
	mux.HandleFunc("/auth/logout", e.logout)
}

// fallback answers what no service and no sign-in route took: the shell plugin's
// page once there is one.
func (e *Edge) fallback(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path != "/" {
		http.NotFound(w, r)
		return
	}
	serveHTML(w, fallbackPage)
}

// link signs a person in with a one-time link, and shows the first admin's form.
func (e *Edge) link(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	token := strings.TrimPrefix(r.URL.Path, "/auth/link/")
	if token == "" {
		http.NotFound(w, r)
		return
	}

	link, err := e.deps.Links.Check(ctx, token)
	if err != nil {
		http.Error(w, "This link has been used or has expired. Ask an admin for another one.", http.StatusNotFound)
		return
	}

	if link.Email == "" {
		if r.Method != http.MethodPost {
			serveHTML(w, setupPage)
			return
		}
		e.setUpFirstAdmin(w, r, token)
		return
	}

	if r.Method != http.MethodGet {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}

	// A link for an email: that person, or their user made now, signs in.
	user, err := e.userForLink(ctx, link)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	if _, err := e.deps.Links.Consume(ctx, token); err != nil {
		http.Error(w, "This link has been used or has expired. Ask an admin for another one.", http.StatusNotFound)
		return
	}
	e.startSession(w, r, user)
}

// userForLink is the user a link signs in.
func (e *Edge) userForLink(ctx context.Context, link store.RookeryLinks) (store.RookeryUsers, error) {
	if link.UserID != nil && *link.UserID != "" {
		return e.deps.Users.ByID(ctx, *link.UserID)
	}
	return e.deps.Users.Ensure(ctx, link.Email, "", e.adminEmail(link.Email))
}

// setUpFirstAdmin makes the first admin from the setup link's form. It refuses
// once there is an admin: a setup link that outlived its use must not make
// another one.
func (e *Edge) setUpFirstAdmin(w http.ResponseWriter, r *http.Request, token string) {
	ctx := r.Context()
	if err := r.ParseForm(); err != nil {
		http.Error(w, "the form could not be read", http.StatusBadRequest)
		return
	}
	email := strings.TrimSpace(r.PostFormValue("email"))
	name := strings.TrimSpace(r.PostFormValue("name"))

	if _, err := e.deps.Links.Check(ctx, token); err != nil {
		http.Error(w, "This link has been used or has expired.", http.StatusNotFound)
		return
	}
	if e.hasAdmin(ctx) {
		http.Error(w, "This deployment has an admin already. Ask them for a link.", http.StatusConflict)
		return
	}
	user, err := e.deps.Users.Ensure(ctx, email, name, true)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	if _, err := e.deps.Users.SetFlags(ctx, user.ID, true, false); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	user, err = e.deps.Users.ByID(ctx, user.ID)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	if _, err := e.deps.Links.Consume(ctx, token); err != nil {
		http.Error(w, "This link has been used or has expired.", http.StatusNotFound)
		return
	}
	e.startSession(w, r, user)
}

// hasAdmin reports whether the deployment has an admin who may sign in.
func (e *Edge) hasAdmin(ctx context.Context) bool {
	admins, err := e.deps.Store.Queries().CountAdmins(ctx)
	return err == nil && admins > 0
}

// adminEmail reports whether an email starts as an admin.
func (e *Edge) adminEmail(email string) bool {
	for _, admin := range e.deps.OIDCAdmins {
		if strings.EqualFold(strings.TrimSpace(admin), strings.TrimSpace(email)) {
			return true
		}
	}
	return false
}

// oidcStart sends the browser to the identity provider.
func (e *Edge) oidcStart(w http.ResponseWriter, r *http.Request) {
	if e.deps.OIDC == nil {
		http.Error(w, "this deployment has no identity provider", http.StatusNotFound)
		return
	}

	state := authn.NewState()
	verifier := authn.NewVerifier()
	attempt, err := json.Marshal(oidcAttempt{
		State:    state,
		Verifier: verifier,
		Next:     safeNext(r.URL.Query().Get("next")),
	})
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	e.setCookie(w, oidcCookie, string(attempt), oidcCookieLifetime, true)
	http.Redirect(w, r, e.deps.OIDC.StartURL(state, verifier), http.StatusFound)
}

// oidcAttempt is what the browser carries through the provider.
type oidcAttempt struct {
	State    string `json:"state"`
	Verifier string `json:"verifier"`
	Next     string `json:"next"`
}

// oidcCallback finishes a sign-in at the identity provider.
func (e *Edge) oidcCallback(w http.ResponseWriter, r *http.Request) {
	if e.deps.OIDC == nil {
		http.Error(w, "this deployment has no identity provider", http.StatusNotFound)
		return
	}
	ctx := r.Context()

	cookie, err := r.Cookie(oidcCookie)
	if err != nil {
		http.Error(w, "start the sign-in again", http.StatusBadRequest)
		return
	}
	e.clearCookie(w, oidcCookie)

	var attempt oidcAttempt
	if err := json.Unmarshal([]byte(cookie.Value), &attempt); err != nil {
		http.Error(w, "start the sign-in again", http.StatusBadRequest)
		return
	}
	if state := r.URL.Query().Get("state"); state != attempt.State || state == "" {
		http.Error(w, "start the sign-in again", http.StatusBadRequest)
		return
	}
	if failure := r.URL.Query().Get("error"); failure != "" {
		http.Error(w, "the identity provider refused: "+failure, http.StatusForbidden)
		return
	}

	identity, err := e.deps.OIDC.Exchange(ctx, r.URL.Query().Get("code"), attempt.Verifier)
	if !errors.Is(err, authn.ErrOIDC) && err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	if err != nil {
		http.Error(w, "the identity provider's answer could not be checked", http.StatusForbidden)
		return
	}
	if identity.Email == "" {
		http.Error(w, "the identity provider gave no email address", http.StatusForbidden)
		return
	}

	user, err := e.deps.Users.Ensure(ctx, identity.Email, identity.Name, e.adminEmail(identity.Email))
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	if user.Disabled {
		http.Error(w, "this account is disabled", http.StatusForbidden)
		return
	}
	if err := e.deps.Groups.SyncClaims(ctx, user.ID, map[string][]string{
		e.deps.OIDC.GroupsClaim(): identity.Groups,
	}); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	e.startSession(w, r, user)
	http.Redirect(w, r, valueOrEmpty(attempt.Next, "/"), http.StatusFound)
}

// cli signs the CLI in. A GET shows the page a browser approves; a form POST
// makes the code the CLI's loopback listener gets; a JSON POST trades the code
// and the PKCE verifier for an access token.
func (e *Edge) cli(w http.ResponseWriter, r *http.Request) {
	switch {
	case r.Method == http.MethodGet:
		e.cliPage(w, r)
	case r.Method == http.MethodPost && strings.HasPrefix(r.Header.Get("Content-Type"), "application/json"):
		e.cliToken(w, r)
	case r.Method == http.MethodPost:
		e.cliApprove(w, r)
	default:
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
	}
}

// cliPage shows the approval form to a signed-in person, and sends anybody else
// through OIDC first.
func (e *Edge) cliPage(w http.ResponseWriter, r *http.Request) {
	query := r.URL.Query()
	challenge := query.Get("challenge")
	port := query.Get("port")
	state := query.Get("state")
	if challenge == "" || port == "" {
		http.Error(w, "the challenge and the port are required", http.StatusBadRequest)
		return
	}

	principal, ok := access.PrincipalFrom(r.Context())
	if !ok {
		if e.deps.OIDC == nil {
			http.Error(w, "sign in at /auth/link/ first, then try again", http.StatusUnauthorized)
			return
		}
		next := r.URL.RequestURI()
		http.Redirect(w, r, "/auth/oidc/start?next="+url.QueryEscape(next), http.StatusFound)
		return
	}

	serveHTML(w, fmt.Sprintf(cliPage, htmlEscape(principal.Email), htmlEscape(challenge), htmlEscape(port), htmlEscape(state)))
}

// cliApprove makes the code the CLI's loopback listener is redirected with.
func (e *Edge) cliApprove(w http.ResponseWriter, r *http.Request) {
	principal, ok := access.PrincipalFrom(r.Context())
	if !ok {
		http.Error(w, "not signed in", http.StatusUnauthorized)
		return
	}
	if err := r.ParseForm(); err != nil {
		http.Error(w, "the form could not be read", http.StatusBadRequest)
		return
	}
	challenge := r.PostFormValue("challenge")
	port := r.PostFormValue("port")
	state := r.PostFormValue("state")
	if challenge == "" || port == "" {
		http.Error(w, "the challenge and the port are required", http.StatusBadRequest)
		return
	}
	if !isLoopbackPort(port) {
		http.Error(w, "the port must be on this machine", http.StatusBadRequest)
		return
	}

	code, err := e.codes.put(principal.ID, challenge)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	target := "http://127.0.0.1:" + port + "/callback?code=" + url.QueryEscape(code)
	if state != "" {
		target += "&state=" + url.QueryEscape(state)
	}
	http.Redirect(w, r, target, http.StatusFound) //nolint:gosec // the port is digits and the rest is escaped
}

// cliTokenJSON is what the CLI gets.
type cliTokenJSON struct {
	Token  string `json:"token"`
	Server string `json:"server"`
}

// cliToken trades a code and its PKCE verifier for an access token.
func (e *Edge) cliToken(w http.ResponseWriter, r *http.Request) {
	var request struct {
		Code     string `json:"code"`
		Verifier string `json:"verifier"`
	}
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 64<<10)).Decode(&request); err != nil {
		http.Error(w, "the request could not be read", http.StatusBadRequest)
		return
	}

	userID, err := e.codes.take(request.Code, request.Verifier)
	if err != nil {
		http.Error(w, err.Error(), http.StatusForbidden)
		return
	}
	secret, _, err := e.deps.Tokens.Create(r.Context(), userID, "rookery login")
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(cliTokenJSON{Token: secret, Server: e.deps.PublicURL})
}

// logout ends the browser's session.
func (e *Edge) logout(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	ctx := r.Context()
	if session, ok := authn.SessionRecordFrom(ctx); ok {
		principal, _ := access.PrincipalFrom(ctx)
		if err := e.deps.Sessions.Revoke(ctx, principal.ID, session.ID); err != nil {
			e.deps.Logger.Debug("revoking a session at logout", "error", err)
		}
	}
	e.clearSession(w)
	http.Redirect(w, r, "/", http.StatusFound)
}

// startSession makes a session for a user and sends the browser home.
func (e *Edge) startSession(w http.ResponseWriter, r *http.Request, user store.RookeryUsers) {
	secret, session, err := e.deps.Sessions.Create(r.Context(), user.ID, r.UserAgent())
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	e.setSession(w, secret, session.Csrf)
	http.Redirect(w, r, valueOrEmpty(safeNext(r.URL.Query().Get("next")), "/"), http.StatusFound) //nolint:gosec // safeNext keeps the path on this server
}

// setSession writes the session's cookies.
func (e *Edge) setSession(w http.ResponseWriter, secret, csrf string) {
	e.setCookie(w, sessionCookie, secret, 30*24*time.Hour, true)
	e.setCookie(w, csrfCookie, csrf, 30*24*time.Hour, false)
}

// clearSession removes the session's cookies.
func (e *Edge) clearSession(w http.ResponseWriter) {
	e.clearCookie(w, sessionCookie)
	e.clearCookie(w, csrfCookie)
}

// setCookie writes one of the edge's cookies.
func (e *Edge) setCookie(w http.ResponseWriter, name, value string, lifetime time.Duration, httpOnly bool) {
	http.SetCookie(w, &http.Cookie{ //nolint:gosec // Secure is the deployment's, HttpOnly is per cookie, SameSite is Lax
		Name:     name,
		Value:    value,
		Path:     "/",
		MaxAge:   int(lifetime.Seconds()),
		HttpOnly: httpOnly,
		Secure:   e.deps.CookieSecure,
		SameSite: http.SameSiteLaxMode,
	})
}

// clearCookie removes a cookie.
func (e *Edge) clearCookie(w http.ResponseWriter, name string) {
	http.SetCookie(w, &http.Cookie{ //nolint:gosec // the same attributes as the cookie it clears
		Name:     name,
		Value:    "",
		Path:     "/",
		MaxAge:   -1,
		HttpOnly: true,
		Secure:   e.deps.CookieSecure,
		SameSite: http.SameSiteLaxMode,
	})
}

// codeStore holds the CLI sign-in codes. They live in memory: an edge that
// restarts forgets them, and the CLI starts again.
type codeStore struct {
	mu    sync.Mutex
	life  time.Duration
	codes map[string]cliCode
}

type cliCode struct {
	userID    string
	challenge string
	expires   time.Time
}

// newCodeStore returns an empty store whose codes live for life.
func newCodeStore(life time.Duration) *codeStore {
	return &codeStore{life: life, codes: make(map[string]cliCode)}
}

// put makes a code for a user and a PKCE challenge.
func (c *codeStore) put(userID, challenge string) (string, error) {
	code := id.Secret(24)
	c.mu.Lock()
	defer c.mu.Unlock()
	c.expireLocked()
	c.codes[code] = cliCode{userID: userID, challenge: challenge, expires: time.Now().Add(c.life)}
	return code, nil
}

// take spends a code, and checks the verifier against its challenge.
func (c *codeStore) take(code, verifier string) (string, error) {
	c.mu.Lock()
	entry, ok := c.codes[code]
	if ok {
		delete(c.codes, code)
	}
	c.mu.Unlock()

	switch {
	case !ok || time.Now().After(entry.expires):
		return "", errors.New("the code is not good: sign in again")
	case verifier == "":
		return "", errors.New("the request has no verifier")
	case subtle.ConstantTimeCompare([]byte(authn.Challenge(verifier)), []byte(entry.challenge)) != 1:
		return "", errors.New("the verifier does not match the challenge")
	}
	return entry.userID, nil
}

// expireLocked drops the codes that have expired.
func (c *codeStore) expireLocked() {
	now := time.Now()
	for code, entry := range c.codes {
		if now.After(entry.expires) {
			delete(c.codes, code)
		}
	}
}

// serveHTML answers with a page.
func serveHTML(w http.ResponseWriter, page string) {
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	_, _ = w.Write([]byte(page)) //nolint:gosec // the pages are constants and escaped values
}

// htmlEscape escapes text for a page.
func htmlEscape(text string) string {
	replacer := strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;", `"`, "&#34;", "'", "&#39;")
	return replacer.Replace(text)
}

// safeNext is a redirect target that stays on this server: a path, not a URL.
func safeNext(next string) string {
	if next == "" || !strings.HasPrefix(next, "/") || strings.HasPrefix(next, "//") {
		return ""
	}
	return next
}

// valueOrEmpty is a value, or a fallback when it is empty.
func valueOrEmpty(value, fallback string) string {
	if value == "" {
		return fallback
	}
	return value
}

// isLoopbackPort reports whether a port is one a loopback listener could be on.
func isLoopbackPort(port string) bool {
	if len(port) == 0 || len(port) > 5 {
		return false
	}
	for _, c := range port {
		if c < '0' || c > '9' {
			return false
		}
	}
	return true
}
