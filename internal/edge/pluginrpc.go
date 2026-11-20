package edge

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/http/httputil"
	"time"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/plugins"
)

// PluginServices starts a plugin's service for a call, and says where it is.
type PluginServices interface {
	Ensure(ctx context.Context, id string) (string, error)
}

// PluginProcedures says what a declared procedure needs, from the manifest.
type PluginProcedures interface {
	// Access is the level and resource kind a procedure declares, and whether
	// the plugin declares it at all.
	Access(ctx context.Context, id, procedure string) (plugins.Level, plugins.Resource, bool, error)
}

// registerPluginRPC serves a plugin's declared procedures at
// /plugins/<id>/rpc/<service>/<method>.
//
// The call is forwarded as it came, to the service on its own socket, after the
// manifest's access check: a plugin declares who may call each procedure, and
// the edge is where that is enforced, since a plugin process must not have to
// repeat the core's access rules.
func (e *Edge) registerPluginRPC(mux *http.ServeMux) {
	if e.deps.Plugins == nil || e.deps.Services == nil || e.deps.Procedures == nil {
		return
	}
	mux.HandleFunc("/plugins/{id}/rpc/{service}/{method}", e.pluginRPC)
}

func (e *Edge) pluginRPC(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	procedure := r.PathValue("service") + "/" + r.PathValue("method")

	principal, ok := access.PrincipalFrom(r.Context())
	if !ok {
		unauthorized(w)
		return
	}
	if err := checkCSRF(r.Context(), r.Header); err != nil {
		http.Error(w, "Invalid request credential.", http.StatusForbidden)
		return
	}
	level, kind, declared, err := e.deps.Procedures.Access(r.Context(), id, procedure)
	if err != nil {
		writePluginError(w, err)
		return
	}
	if !declared {
		http.Error(w, "This plugin has no such procedure.", http.StatusNotFound)
		return
	}

	resource, err := accessResource(r, id, kind)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	need, err := access.ParseLevel(string(level))
	if err != nil {
		http.Error(w, "This plugin declares an access level Rookery does not know.", http.StatusInternalServerError)
		return
	}
	checker := access.NewChecker(e.deps.Store)
	if err := checker.Check(r.Context(), principal, resource, need); err != nil {
		writePluginError(w, err)
		return
	}

	socket, err := e.deps.Services.Ensure(r.Context(), id)
	if err != nil {
		writePluginError(w, err)
		return
	}

	transport := &http.Transport{
		DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
			var dialer net.Dialer
			return dialer.DialContext(ctx, "unix", socket)
		},
		MaxIdleConns:    2,
		IdleConnTimeout: time.Minute,
	}
	defer transport.CloseIdleConnections()
	if services, ok := e.deps.Services.(interface{ HTTP2(string) bool }); ok && services.HTTP2(id) {
		protocols := new(http.Protocols)
		protocols.SetUnencryptedHTTP2(true)
		transport.Protocols = protocols
	}

	var caller string
	if principal.Kind == access.KindUser && e.deps.Callers != nil {
		caller = e.deps.Callers.Issue(id, principal.ID, r.Header.Get("Rookery-Space"), time.Time{})
	}
	proxy := &httputil.ReverseProxy{
		Transport: transport,
		Rewrite: func(req *httputil.ProxyRequest) {
			req.Out.URL.Scheme, req.Out.URL.Host = "http", "plugin"
			req.Out.URL.Path, req.Out.URL.RawPath = "/"+procedure, ""
			req.Out.Host = "plugin"
			req.Out.Header.Del("Authorization")
			req.Out.Header.Del("Cookie")
			req.Out.Header.Del(CSRFHeader)
			req.Out.Header.Del(access.CallerTokenHeader)
			if caller != "" {
				req.Out.Header.Set(access.CallerTokenHeader, caller)
			}
		},
		ModifyResponse: func(response *http.Response) error {
			response.Header.Del(access.CallerTokenHeader)
			response.Header.Del("Set-Cookie")
			return nil
		},
		ErrorHandler: func(w http.ResponseWriter, _ *http.Request, err error) {
			writePluginError(w, err)
		},
	}
	proxy.ServeHTTP(w, r)
}

// accessResource is the resource a declared procedure's access is checked on.
func accessResource(r *http.Request, pluginID string, kind plugins.Resource) (string, error) {
	switch kind {
	case plugins.ResourceSpace, plugins.ResourceHost, plugins.ResourceRecord, plugins.ResourceDocument, plugins.ResourceProvisioner, plugins.ResourceRollout:
		value := r.Header.Get("Rookery-" + string(kind))
		if value == "" {
			return "", fmt.Errorf("the procedure needs the Rookery-%s header", kind)
		}
		resource := string(kind) + "://" + value
		if kind == plugins.ResourceRecord {
			resource = value
		}
		resolved, err := access.ParseResource(resource)
		if err != nil || resolved != string(kind) {
			return "", fmt.Errorf("the Rookery-%s header has an invalid resource", kind)
		}
		return resource, nil
	case plugins.ResourcePlugin:
		return access.PluginURI(pluginID), nil
	case "":
		return "", nil
	default:
		return "", fmt.Errorf("the plugin declares access on %s, which the edge does not resolve", kind)
	}
}

// writePluginError answers a refused or failed plugin call.
func writePluginError(w http.ResponseWriter, err error) {
	var denied *access.Denied
	switch {
	case errors.Is(err, access.ErrUnauthenticated):
		unauthorized(w)
	case errors.As(err, &denied), errors.Is(err, access.ErrDenied):
		http.Error(w, "You may not call this.", http.StatusForbidden)
	case errors.Is(err, access.ErrNotFound):
		http.Error(w, "This plugin is not installed.", http.StatusNotFound)
	default:
		http.Error(w, "The plugin cannot be called.", http.StatusInternalServerError)
	}
}

// PluginTokens is the plugin whose service holds a token.
type PluginTokens interface {
	PluginByToken(token string) (string, bool)
}
