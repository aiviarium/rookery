// Package streamio supports bidirectional RPC handlers that must stop reading
// when the upstream closes first.
package streamio

import (
	"context"
	"net/http"
)

type bodyKey struct{}

// WithReadInterrupt makes the request body available to InterruptReads. The
// HTTP/2 request body permits Close concurrent with Read.
func WithReadInterrupt(handler http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		ctx := context.WithValue(r.Context(), bodyKey{}, r.Body)
		handler.ServeHTTP(w, r.WithContext(ctx))
	})
}

// InterruptReads unblocks a pending Receive without preventing the handler from
// sending its final status.
func InterruptReads(ctx context.Context) {
	if body, ok := ctx.Value(bodyKey{}).(interface{ Close() error }); ok {
		_ = body.Close()
	}
}
