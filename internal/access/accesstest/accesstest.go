// Package accesstest gives a service test a real connect server and client with
// the access interceptor in front, the way the edge has them.
//
// The test names the caller. What the edge's authentication middleware does with
// a cookie, a token or the Unix socket is its own test's business; here the
// principal is simply what every call is made as.
package accesstest

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"

	"connectrpc.com/connect"

	"github.com/aiviarium/rookery/internal/access"
)

// Server is a test's server, with the handlers it registered.
type Server struct {
	*httptest.Server

	// Decorate adds to every call's context before the handlers see it, for
	// what the edge's middleware would put there: a session, a space.
	Decorate func(context.Context) context.Context

	http *http.Client
}

// New starts a server whose calls are made as principal, with the access
// interceptor on every handler register adds. register is called with the
// handler options the handlers need.
//
// outer are handler options that come before the access interceptor, such as the
// audit interceptor: connect runs them outermost, so they see a call the access
// interceptor refused.
func New(t *testing.T, checker *access.Checker, principal access.Principal, register func(mux *http.ServeMux, options []connect.HandlerOption), outer ...connect.HandlerOption) *Server {
	t.Helper()

	mux := http.NewServeMux()
	options := make([]connect.HandlerOption, 0, len(outer)+1)
	options = append(options, outer...)
	options = append(options, connect.WithInterceptors(access.NewInterceptor(checker)))
	register(mux, options)

	wrapped := &Server{}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		ctx := r.Context()
		if principal.Authenticated() {
			ctx = access.WithPrincipal(ctx, principal)
		}
		if wrapped.Decorate != nil {
			ctx = wrapped.Decorate(ctx)
		}
		mux.ServeHTTP(w, r.WithContext(ctx))
	}))
	t.Cleanup(server.Close)

	wrapped.Server = server
	wrapped.http = server.Client()
	return wrapped
}

// Client is a connect client for one procedure, speaking gRPC like Rookery's own
// processes do.
func Client[Req, Res any](server *Server, path string) *connect.Client[Req, Res] {
	return connect.NewClient[Req, Res](server.http, server.URL+path, connect.WithGRPC())
}

// Anonymous is a caller who did not sign in.
func Anonymous() access.Principal { return access.Principal{} }

// User is a caller who is a user, by id, admin or not.
func User(id string, admin bool) access.Principal {
	return access.Principal{Kind: access.KindUser, ID: id, Email: id + "@example.test", Admin: admin}
}

// Local is the Unix socket's admin.
func Local() access.Principal { return access.LocalAdmin() }

// Call makes a unary call in a test, and fails it with the error a handler
// returned.
func Call[Req, Res any](ctx context.Context, client *connect.Client[Req, Res], request *Req) (*Res, error) {
	response, err := client.CallUnary(ctx, connect.NewRequest(request))
	if err != nil {
		return nil, err
	}
	return response.Msg, nil
}
