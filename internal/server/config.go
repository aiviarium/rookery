// Package server is the running Rookery: what it reads from its environment, the
// leader lock that makes one server of several the leader, the listeners, and the
// wiring of every package in one place.
//
// A server that is not the leader is a standby: it answers /healthz with 503 and
// waits for the lock. When the leader's lock connection drops, it exits, and a
// process manager starts it again as a standby.
package server

import (
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

// Config is what a server is configured with, read once from ROOKERY_* variables.
type Config struct {
	// DatabaseURL is ROOKERY_DATABASE_URL: the Postgres every server uses.
	DatabaseURL string
	// Version is the build's version, which the applied digest is taken over.
	Version string
	// BundlesDir is where the first-party plugin packages are shipped.
	BundlesDir string

	// DatabaseLockURL is ROOKERY_DATABASE_LOCK_URL: where the leader lock is
	// taken when DatabaseURL goes through a pooler that would defeat it. Empty
	// takes the lock on DatabaseURL.
	DatabaseLockURL string

	// Listen is ROOKERY_LISTEN, the edge's public address.
	Listen string
	// Tailnet fields configure the leader's tsnet node and edge listener.
	TailnetListen        string
	TailnetHostname      string
	TailnetName          string
	TailnetHeadscaleUser string
	// TLSCert and TLSKey turn the edge into HTTPS when both are set; otherwise
	// it speaks h2c.
	TLSCert string
	TLSKey  string
	// Socket is ROOKERY_SOCKET, the Unix socket the CLI and hosts' programs
	// use, served as its local admin.
	Socket string
	// SocketMode is the Unix socket's mode, 0660: the server's group.
	SocketMode os.FileMode

	// PublicURL is how clients reach the edge, for links and OIDC's redirect.
	PublicURL string

	// SecretKey is ROOKERY_SECRET_KEY, 64 hex digits, which wins over the file.
	SecretKey string
	// SecretKeyFile is ROOKERY_SECRET_KEY_FILE, created on an empty database.
	SecretKeyFile string
	// PreviousKeyFile contains the old key during a deployment rekey.
	PreviousKeyFile string

	// StateDir is ROOKERY_STATE_DIR: work directories, plugin folders, the
	// tailnet's state.
	StateDir string
	// ReleaseDir is ROOKERY_RELEASE_DIR, the folder of host binaries.
	ReleaseDir string
	// Blobs is ROOKERY_BLOBS: a directory, or s3://bucket/prefix.
	Blobs string
	// S3Endpoint, S3AccessKey, S3SecretKey and S3Region reach the bucket, and
	// S3Secure and S3PathStyle say how.
	S3Endpoint  string
	S3AccessKey string
	S3SecretKey string
	S3Region    string
	S3Secure    bool
	S3PathStyle bool

	// Admins is ROOKERY_ADMINS: emails that start as admins on first sign-in.
	Admins []string
	// OIDCIssuer, OIDCClientID and OIDCClientSecret turn OIDC on when the
	// issuer and the client id are set.
	OIDCIssuer       string
	OIDCClientID     string
	OIDCClientSecret string
	// OIDCGroupsClaim is ROOKERY_OIDC_GROUPS_CLAIM, "groups" by default.
	OIDCGroupsClaim string

	// RetentionDays is ROOKERY_RETENTION_DAYS: how long job records and lines
	// are kept.
	RetentionDays int
	// CookieSecure is ROOKERY_COOKIE_SECURE, true by default: a session cookie
	// over plain HTTP is only for a deployment that terminates TLS in front.
	CookieSecure bool
	// LogLevel is ROOKERY_LOG_LEVEL: debug, info, warn or error.
	LogLevel slog.Level
}

// Defaults.
const (
	DefaultListen        = "127.0.0.1:4770"
	DefaultSocket        = "/run/rookery/rookery.sock"
	DefaultStateDir      = "/var/lib/rookery"
	DefaultRetentionDays = 90
)

// Load reads the configuration from the environment.
func Load() (Config, error) { return LoadFrom(os.Getenv) }

// LoadFrom reads the configuration through get, which a test supplies.
func LoadFrom(get func(string) string) (Config, error) {
	config := Config{
		DatabaseURL:          get("ROOKERY_DATABASE_URL"),
		DatabaseLockURL:      get("ROOKERY_DATABASE_LOCK_URL"),
		Listen:               valueOr(get("ROOKERY_LISTEN"), DefaultListen),
		TailnetListen:        get("ROOKERY_TAILNET_LISTEN"),
		TailnetHostname:      get("ROOKERY_TAILNET_HOSTNAME"),
		TailnetName:          get("ROOKERY_TAILNET_NAME"),
		TailnetHeadscaleUser: get("ROOKERY_TAILNET_HEADSCALE_USER"),
		TLSCert:              get("ROOKERY_TLS_CERT"),
		TLSKey:               get("ROOKERY_TLS_KEY"),
		Socket:               valueOr(get("ROOKERY_SOCKET"), DefaultSocket),
		SocketMode:           0o660,
		PublicURL:            strings.TrimRight(get("ROOKERY_PUBLIC_URL"), "/"),
		SecretKey:            get("ROOKERY_SECRET_KEY"),
		SecretKeyFile:        get("ROOKERY_SECRET_KEY_FILE"),
		BundlesDir:           get("ROOKERY_BUNDLES_DIR"),
		PreviousKeyFile:      get("ROOKERY_SECRET_PREVIOUS_KEY_FILE"),
		StateDir:             valueOr(get("ROOKERY_STATE_DIR"), DefaultStateDir),
		ReleaseDir:           get("ROOKERY_RELEASE_DIR"),
		Blobs:                get("ROOKERY_BLOBS"),
		S3Endpoint:           get("ROOKERY_S3_ENDPOINT"),
		S3AccessKey:          get("ROOKERY_S3_ACCESS_KEY"),
		S3SecretKey:          get("ROOKERY_S3_SECRET_KEY"),
		S3Region:             get("ROOKERY_S3_REGION"),
		Admins:               splitList(get("ROOKERY_ADMINS")),
		OIDCIssuer:           get("ROOKERY_OIDC_ISSUER"),
		OIDCClientID:         get("ROOKERY_OIDC_CLIENT_ID"),
		OIDCClientSecret:     get("ROOKERY_OIDC_CLIENT_SECRET"),
		OIDCGroupsClaim:      valueOr(get("ROOKERY_OIDC_GROUPS_CLAIM"), "groups"),
		RetentionDays:        DefaultRetentionDays,
		CookieSecure:         true,
	}

	if config.BundlesDir == "" {
		if executable, err := os.Executable(); err == nil {
			config.BundlesDir = filepath.Join(filepath.Dir(executable), "plugins")
		}
	}

	if config.DatabaseURL == "" {
		return Config{}, fmt.Errorf("server: ROOKERY_DATABASE_URL is not set")
	}
	if config.DatabaseLockURL == "" {
		config.DatabaseLockURL = config.DatabaseURL
	}
	if config.SecretKey == "" && config.SecretKeyFile == "" {
		config.SecretKeyFile = filepath.Join(config.StateDir, "secret.key")
	}
	if config.Blobs == "" {
		config.Blobs = filepath.Join(config.StateDir, "blobs")
	}
	if config.PublicURL == "" {
		config.PublicURL = defaultPublicURL(config.Listen)
	}

	if raw := get("ROOKERY_RETENTION_DAYS"); raw != "" {
		days, err := strconv.Atoi(raw)
		if err != nil || days < 1 {
			return Config{}, fmt.Errorf("server: ROOKERY_RETENTION_DAYS is %q, not a number of days", raw)
		}
		config.RetentionDays = days
	}
	if raw := get("ROOKERY_S3_SECURE"); raw != "" {
		secure, err := strconv.ParseBool(raw)
		if err != nil {
			return Config{}, fmt.Errorf("server: ROOKERY_S3_SECURE is %q, not true or false", raw)
		}
		config.S3Secure = secure
	}
	if raw := get("ROOKERY_S3_PATH_STYLE"); raw != "" {
		pathStyle, err := strconv.ParseBool(raw)
		if err != nil {
			return Config{}, fmt.Errorf("server: ROOKERY_S3_PATH_STYLE is %q, not true or false", raw)
		}
		config.S3PathStyle = pathStyle
	}
	if raw := get("ROOKERY_COOKIE_SECURE"); raw != "" {
		secure, err := strconv.ParseBool(raw)
		if err != nil {
			return Config{}, fmt.Errorf("server: ROOKERY_COOKIE_SECURE is %q, not true or false", raw)
		}
		config.CookieSecure = secure
	}
	level, err := parseLevel(get("ROOKERY_LOG_LEVEL"))
	if err != nil {
		return Config{}, err
	}
	config.LogLevel = level

	return config, nil
}

// Logger is the server's logger at the configured level.
func (c Config) Logger() *slog.Logger {
	return slog.New(slog.NewJSONHandler(os.Stderr, &slog.HandlerOptions{Level: c.LogLevel}))
}

// OIDCEnabled reports whether the deployment signs in at an identity provider.
func (c Config) OIDCEnabled() bool {
	return c.OIDCIssuer != "" && c.OIDCClientID != ""
}

// TLSEnabled reports whether the edge serves HTTPS.
func (c Config) TLSEnabled() bool { return c.TLSCert != "" && c.TLSKey != "" }

// valueOr is a variable's value, or a default when it is empty.
func valueOr(value, fallback string) string {
	if strings.TrimSpace(value) == "" {
		return fallback
	}
	return value
}

// splitList reads a comma-separated list.
func splitList(raw string) []string {
	if strings.TrimSpace(raw) == "" {
		return nil
	}
	parts := strings.Split(raw, ",")
	out := make([]string, 0, len(parts))
	for _, part := range parts {
		if part = strings.TrimSpace(part); part != "" {
			out = append(out, part)
		}
	}
	return out
}

// parseLevel reads a log level.
func parseLevel(raw string) (slog.Level, error) {
	switch strings.ToLower(strings.TrimSpace(raw)) {
	case "", "info":
		return slog.LevelInfo, nil
	case "debug":
		return slog.LevelDebug, nil
	case "warn", "warning":
		return slog.LevelWarn, nil
	case "error":
		return slog.LevelError, nil
	default:
		return 0, fmt.Errorf("server: ROOKERY_LOG_LEVEL is %q, not debug, info, warn or error", raw)
	}
}

// defaultPublicURL is the address a browser is told the edge is at, from the
// listen address.
func defaultPublicURL(listen string) string {
	host, port, found := strings.Cut(listen, ":")
	scheme := "http"
	switch {
	case !found:
		return "http://" + listen
	case host == "" || host == "0.0.0.0" || host == "::":
		host = "127.0.0.1"
	}
	return scheme + "://" + host + ":" + port
}
