package sdk

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"

	"golang.org/x/sync/errgroup"
)

// Serve serves handler on the Unix socket at socket, speaking gRPC: HTTP/2
// without TLS, which is what Rookery's own processes use with each other.
//
// It returns when ctx is done, after the socket is closed and removed. A socket
// another process left behind is refused: only one process can serve on one.
func Serve(ctx context.Context, socket string, handler http.Handler) error {
	listener, err := listenUnix(ctx, socket)
	if err != nil {
		return err
	}
	defer func() {
		_ = listener.Close()
		_ = os.Remove(socket)
	}()

	protocols := new(http.Protocols)
	protocols.SetUnencryptedHTTP2(true)
	server := &http.Server{
		Handler:           handler,
		Protocols:         protocols,
		ReadHeaderTimeout: time.Minute,
		ErrorLog:          slog.NewLogLogger(slog.Default().Handler(), slog.LevelDebug),
		// The socket's life is ctx's: a call in flight when the server goes is
		// ended with it, rather than holding the shutdown up.
		BaseContext: func(net.Listener) context.Context { return ctx },
	}

	group, groupCtx := errgroup.WithContext(ctx)
	group.Go(func() error {
		err := server.Serve(listener)
		if errors.Is(err, http.ErrServerClosed) || errors.Is(err, net.ErrClosed) {
			return nil
		}
		return fmt.Errorf("sdk: serving %s: %w", socket, err)
	})
	group.Go(func() error {
		<-groupCtx.Done()
		// The process is going: a call in flight is ended with it rather than
		// holding its exit up. The server stops a plugin with a signal, and
		// waits for the process, not for the call.
		if err := server.Close(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			return fmt.Errorf("sdk: stopping the server on %s: %w", socket, err)
		}
		return nil
	})
	return group.Wait()
}

// listenUnix listens on a Unix socket, removing the one an exited process left.
func listenUnix(ctx context.Context, socket string) (net.Listener, error) {
	if err := os.MkdirAll(filepath.Dir(socket), 0o700); err != nil {
		return nil, fmt.Errorf("sdk: making the socket's directory: %w", err)
	}
	if info, err := os.Lstat(socket); err == nil {
		if info.Mode()&os.ModeSocket == 0 {
			return nil, fmt.Errorf("%w: %s is there and is not a socket", ErrConfig, socket)
		}
		if err := os.Remove(socket); err != nil {
			return nil, fmt.Errorf("sdk: removing the stale socket %s: %w", socket, err)
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return nil, fmt.Errorf("sdk: reading %s: %w", socket, err)
	}

	var listenConfig net.ListenConfig
	listener, err := listenConfig.Listen(ctx, "unix", socket)
	if err != nil {
		return nil, fmt.Errorf("sdk: listening on %s: %w", socket, err)
	}
	// The server and its plugins run as the same user, so only that user may
	// reach it.
	//nolint:gosec // 0600 is the point: the plugin's socket is its own user's.
	if err := os.Chmod(socket, 0o600); err != nil {
		_ = listener.Close()
		return nil, fmt.Errorf("sdk: setting the mode of %s: %w", socket, err)
	}
	return listener, nil
}

// signalContext is cancelled by SIGTERM and SIGINT, which is how the server stops
// a plugin process.
func signalContext() (context.Context, context.CancelFunc) {
	return signal.NotifyContext(context.Background(), syscall.SIGTERM, syscall.SIGINT)
}
