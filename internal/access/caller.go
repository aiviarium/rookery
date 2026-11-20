package access

import (
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"strings"
	"time"

	"github.com/aiviarium/rookery/internal/id"
)

// CallerTokenHeader carries a delegated user identity between a plugin and
// Rookery. The plugin still authenticates with its own process credential.
const CallerTokenHeader = "Rookery-Caller-Token" //nolint:gosec // A header name.

const callerLifetime = 5 * time.Minute

// CallerTokens issues short-lived credentials bound to one plugin.
type CallerTokens struct {
	key []byte
}

type callerClaims struct {
	Plugin string `json:"p"`
	User   string `json:"u"`
	Space  string `json:"s,omitempty"`
	Expiry int64  `json:"e"`
}

// NewCallerTokens returns a signer for the current leader's lifetime.
func NewCallerTokens() *CallerTokens {
	return &CallerTokens{key: []byte(id.Secret(32))}
}

// Issue delegates a user and their current space to the named plugin.
func (c *CallerTokens) Issue(plugin, user, space string, expires time.Time) string {
	if plugin == "" || user == "" {
		return ""
	}
	expiry := time.Now().Add(callerLifetime)
	if !expires.IsZero() && expires.Before(expiry) {
		expiry = expires
	}
	value, _ := json.Marshal(callerClaims{Plugin: plugin, User: user, Space: space, Expiry: expiry.Unix()})
	encoded := base64.RawURLEncoding.EncodeToString(value)
	return encoded + "." + base64.RawURLEncoding.EncodeToString(c.sign(encoded))
}

// Verify checks the intended audience and expiry, returning the user and space.
func (c *CallerTokens) Verify(plugin, token string) (user, space string, err error) {
	if len(token) > 4096 {
		return "", "", ErrUnauthenticated
	}
	encoded, signature, found := strings.Cut(token, ".")
	mac, decodeErr := base64.RawURLEncoding.DecodeString(signature)
	if !found || decodeErr != nil || !hmac.Equal(c.sign(encoded), mac) {
		return "", "", ErrUnauthenticated
	}
	value, decodeErr := base64.RawURLEncoding.DecodeString(encoded)
	var claims callerClaims
	if decodeErr != nil || json.Unmarshal(value, &claims) != nil || claims.Plugin != plugin ||
		claims.User == "" || time.Now().Unix() >= claims.Expiry {
		return "", "", ErrUnauthenticated
	}
	return claims.User, claims.Space, nil
}

func (c *CallerTokens) sign(value string) []byte {
	mac := hmac.New(sha256.New, c.key)
	_, _ = mac.Write([]byte(value))
	return mac.Sum(nil)
}

type pluginIdentityKey struct{}

// WithPluginIdentity records which plugin process authenticated to the socket.
func WithPluginIdentity(ctx context.Context, plugin string) context.Context {
	return context.WithValue(ctx, pluginIdentityKey{}, plugin)
}

// PluginIdentity returns the authenticated plugin even when it delegates a user.
func PluginIdentity(ctx context.Context) (string, bool) {
	if plugin, ok := ctx.Value(pluginIdentityKey{}).(string); ok && plugin != "" {
		return plugin, true
	}
	principal, ok := PrincipalFrom(ctx)
	return principal.ID, ok && principal.Kind == KindPluginPrincipal
}

type provisionerNodeKey struct{}

// WithProvisionerNode attaches the node restriction of a provisioner credential.
func WithProvisionerNode(ctx context.Context, node string) context.Context {
	return context.WithValue(ctx, provisionerNodeKey{}, node)
}

// ProvisionerNode returns the capability of a provisioner process, if any.
func ProvisionerNode(ctx context.Context) (string, bool) {
	node, ok := ctx.Value(provisionerNodeKey{}).(string)
	return node, ok
}
