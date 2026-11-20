package server

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"sync/atomic"
	"time"

	"github.com/aiviarium/rookery/internal/blobs"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/store"
)

// shutdownTimeout is how long a stopping server waits for the calls it has.
const shutdownTimeout = 10 * time.Second

// Server is one Rookery process: a leader, or a standby waiting for the lock.
type Server struct {
	config Config
	log    *slog.Logger

	// healthy is set while this process is the leader and its handlers are
	// wired.
	healthy atomic.Bool
	// wired is the leader's handlers, and nil while this is a standby.
	wired atomic.Pointer[handlers]
}

// New returns a server for a configuration.
func New(config Config, logger *slog.Logger) *Server {
	if logger == nil {
		logger = config.Logger()
	}
	return &Server{config: config, log: logger}
}

// Config is the configuration this server was built with.
func (s *Server) Config() Config { return s.config }

// Run serves until ctx ends, or until this server loses the leader lock. Losing
// the lock is fatal: a standby cannot take over while this process still believes
// it leads.
func (s *Server) Run(ctx context.Context) error {
	opened, err := store.Open(ctx, store.Config{URL: s.config.DatabaseURL})
	if err != nil {
		return err
	}
	defer opened.Close()

	servers, err := s.listen(ctx)
	if err != nil {
		return err
	}
	defer s.stop(ctx, servers)

	leader, err := Acquire(ctx, s.config.DatabaseLockURL, s.log)
	if err != nil {
		return err
	}
	defer leader.Close(context.WithoutCancel(ctx))

	// As leader: the schema, then the sealing key, then everything that talks
	// to the database.
	if err := opened.Ensure(ctx); err != nil {
		return err
	}
	wired, err := s.wire(ctx, opened)
	if err != nil {
		return err
	}
	s.wired.Store(wired)
	s.healthy.Store(true)
	s.log.Info("this server is the leader",
		slog.String("listen", s.config.Listen),
		slog.String("socket", s.config.Socket),
		slog.String("public_url", s.config.PublicURL),
	)

	// The leader's other loops: the job runner, the scheduler and retention.
	loopsCtx, stopLoops := context.WithCancel(ctx)
	loops := make(chan error, 1)
	go func() { loops <- wired.start(loopsCtx) }()

	var stopped error
	select {
	case <-ctx.Done():
		s.log.Info("stopping")
	case err := <-leader.Watch(ctx):
		stopped = err
	}

	s.healthy.Store(false)
	stopLoops()
	if err := <-loops; err != nil {
		s.log.Debug("a leader loop stopped", slog.String("error", err.Error()))
	}
	return stopped
}

// handlers are the two listeners' handlers, and what the leader runs besides
// them.
type handlers struct {
	edge   http.Handler
	socket http.Handler

	log       *slog.Logger
	store     *store.Store
	blobs     *blobs.Blobs
	runner    *jobs.Runner
	scheduler *jobs.Scheduler
	retention time.Duration
}

// logger is the server's logger, for the loops the handlers own.
func (h *handlers) logger() *slog.Logger { return h.log }

// retentionCheck is how often old job records are looked for.
const retentionCheck = 6 * time.Hour

// edgeServer is the HTTP handler of the edge listener.
func (s *Server) edgeServer() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		s.serve(w, r, func(wired *handlers) http.Handler { return wired.edge })
	})
}

// socketServer is the HTTP handler of the Unix socket.
func (s *Server) socketServer() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		s.serve(w, r, func(wired *handlers) http.Handler { return wired.socket })
	})
}

// serve answers a call: /healthz with this server's state, the leader's handler
// when it leads, and 503 otherwise.
func (s *Server) serve(w http.ResponseWriter, r *http.Request, pick func(*handlers) http.Handler) {
	if r.URL.Path == "/healthz" {
		if s.healthy.Load() {
			w.Header().Set("Content-Type", "text/plain; charset=utf-8")
			_, _ = w.Write([]byte("ok\n"))
			return
		}
		http.Error(w, "standby\n", http.StatusServiceUnavailable)
		return
	}

	wired := s.wired.Load()
	if !s.healthy.Load() || wired == nil {
		http.Error(w, "standby\n", http.StatusServiceUnavailable)
		return
	}
	pick(wired).ServeHTTP(w, r)
}

// listen starts the edge and the Unix socket, both answering as a standby until
// this server is the leader.
func (s *Server) listen(ctx context.Context) ([]*http.Server, error) {
	var listenConfig net.ListenConfig
	edgeListener, err := listenConfig.Listen(ctx, "tcp", s.config.Listen)
	if err != nil {
		return nil, fmt.Errorf("server: listening on %s: %w", s.config.Listen, err)
	}
	edge, err := s.newHTTPServer(edgeListener, s.config.TLSEnabled(), s.edgeServer())
	if err != nil {
		_ = edgeListener.Close()
		return nil, err
	}

	socket, err := listenSocket(ctx, s.config.Socket, s.config.SocketMode)
	if err != nil {
		_ = edgeListener.Close()
		return nil, err
	}
	unix, err := s.newHTTPServer(socket, false, s.socketServer())
	if err != nil {
		_ = socket.Close()
		_ = edgeListener.Close()
		return nil, err
	}
	return []*http.Server{edge, unix}, nil
}

// newHTTPServer serves a listener with h2c, or with TLS when useTLS is set. It
// returns once the server is listening, and serves in a goroutine whose owner is
// stop.
func (s *Server) newHTTPServer(listener net.Listener, useTLS bool, handler http.Handler) (*http.Server, error) {
	protocols := new(http.Protocols)
	protocols.SetHTTP1(true)
	if useTLS {
		protocols.SetHTTP2(true)
	} else {
		protocols.SetUnencryptedHTTP2(true)
	}

	server := &http.Server{
		Handler:           handler,
		Protocols:         protocols,
		ReadHeaderTimeout: 15 * time.Second,
	}
	if useTLS {
		certificate, err := tlsConfig(s.config.TLSCert, s.config.TLSKey)
		if err != nil {
			return nil, err
		}
		server.TLSConfig = certificate
	}

	address := listener.Addr().String()
	go func() {
		var err error
		if useTLS {
			err = server.ServeTLS(listener, "", "")
		} else {
			err = server.Serve(listener)
		}
		if err != nil && !errors.Is(err, http.ErrServerClosed) {
			s.log.Error("a listener stopped", slog.String("address", address), slog.String("error", err.Error()))
		}
	}()
	return server, nil
}

// stop closes the listeners and the connections on them, after the run's own
// context has ended.
func (s *Server) stop(ctx context.Context, servers []*http.Server) {
	ctx, cancel := context.WithTimeout(context.WithoutCancel(ctx), shutdownTimeout)
	defer cancel()
	for _, server := range servers {
		if err := server.Shutdown(ctx); err != nil {
			s.log.Debug("shutting a listener down", slog.String("error", err.Error()))
		}
	}
}

// listenSocket opens the Unix socket, mode 0660. A socket file left by a stopped
// server is replaced; anything else at the path is refused.
func listenSocket(ctx context.Context, path string, mode os.FileMode) (net.Listener, error) {
	if err := os.MkdirAll(filepath.Dir(path), 0o750); err != nil {
		return nil, fmt.Errorf("server: making the socket's directory: %w", err)
	}
	if info, err := os.Lstat(path); err == nil {
		if info.Mode()&os.ModeSocket == 0 {
			return nil, fmt.Errorf("server: %s is there and is not a socket", path)
		}
		if err := os.Remove(path); err != nil {
			return nil, fmt.Errorf("server: replacing the socket %s: %w", path, err)
		}
	}

	var listenConfig net.ListenConfig
	listener, err := listenConfig.Listen(ctx, "unix", path)
	if err != nil {
		return nil, fmt.Errorf("server: listening on %s: %w", path, err)
	}
	if err := os.Chmod(path, mode); err != nil {
		_ = listener.Close()
		return nil, fmt.Errorf("server: setting the socket's mode: %w", err)
	}
	return listener, nil
}

// tlsConfig reads the edge's certificate and key.
func tlsConfig(certFile, keyFile string) (*tls.Config, error) {
	certificate, err := tls.LoadX509KeyPair(certFile, keyFile)
	if err != nil {
		return nil, fmt.Errorf("server: reading the edge's certificate: %w", err)
	}
	return &tls.Config{
		Certificates: []tls.Certificate{certificate},
		MinVersion:   tls.VersionTLS12,
		NextProtos:   []string{"h2", "http/1.1"},
	}, nil
}
