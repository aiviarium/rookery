package desktop

import (
	"context"
	"errors"
	"io"
	"net/http"

	"connectrpc.com/connect"
	"github.com/coder/websocket"
	"golang.org/x/sync/errgroup"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/link"
)

// WebSocket returns the browser's RFB handler. The edge supplies the
// authenticated principal through the request context.
func (s *Service) WebSocket() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		ctx := r.Context()
		host := r.PathValue("name")
		if err := s.authorizeViewer(ctx, host); err != nil {
			writeUpgradeError(w, err)
			return
		}

		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{Subprotocols: []string{"binary"}})
		if err != nil {
			return
		}
		defer func() { _ = conn.CloseNow() }()
		conn.SetReadLimit(1 << 20)

		ctx, cancel := context.WithCancel(ctx)
		defer cancel()
		desktop, err := s.openDesktop(ctx, cancel, host)
		if err != nil {
			_ = conn.Close(websocket.StatusTryAgainLater, "the desktop is unavailable")
			return
		}
		defer desktop.Close()

		relayWebSocket(ctx, cancel, conn, desktop)
	})
}

func relayWebSocket(ctx context.Context, cancel context.CancelFunc, conn *websocket.Conn, desktop *link.DesktopStream) {
	group, groupCtx := errgroup.WithContext(ctx)
	stop := context.AfterFunc(groupCtx, cancel)
	defer stop()
	group.Go(func() error {
		for {
			kind, data, err := conn.Read(groupCtx)
			if err != nil {
				return err
			}
			if kind != websocket.MessageBinary {
				err := conn.Close(websocket.StatusUnsupportedData, "the desktop uses binary messages")
				cancel()
				return err
			}
			if err := desktop.Send(data); err != nil {
				if errors.Is(err, io.EOF) {
					return nil
				}
				return err
			}
		}
	})
	group.Go(func() error {
		defer cancel()
		for {
			data, err := desktop.Receive()
			if errors.Is(err, io.EOF) {
				return conn.Close(websocket.StatusNormalClosure, "")
			}
			if err != nil {
				if groupCtx.Err() != nil {
					return nil
				}
				return conn.Close(websocket.StatusInternalError, "the desktop connection ended")
			}
			if err := conn.Write(groupCtx, websocket.MessageBinary, data); err != nil {
				return err
			}
		}
	})
	_ = group.Wait()
}

func writeUpgradeError(w http.ResponseWriter, err error) {
	status := http.StatusInternalServerError
	switch connect.CodeOf(access.ToConnect(err)) {
	case connect.CodeUnauthenticated:
		status = http.StatusUnauthorized
	case connect.CodePermissionDenied:
		status = http.StatusForbidden
	case connect.CodeNotFound:
		status = http.StatusNotFound
	case connect.CodeInvalidArgument:
		status = http.StatusBadRequest
	case connect.CodeUnavailable:
		status = http.StatusServiceUnavailable
	}
	http.Error(w, http.StatusText(status), status)
}
