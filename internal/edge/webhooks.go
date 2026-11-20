package edge

import (
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/hex"
	"errors"
	"io"
	"net/http"
	"strings"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/plugins"
)

const maxWebhookBody = 4 << 20

// WebhookDeclarations finds a webhook belonging to an enabled plugin.
type WebhookDeclarations interface {
	Webhook(ctx context.Context, pluginID, name string) (plugins.Webhook, error)
}

// WebhookSecrets reads the secret in the plugin's scope.
type WebhookSecrets interface {
	Value(ctx context.Context, name, scope string) ([]byte, error)
}

// WebhookJobs enqueues verified work.
type WebhookJobs interface {
	EnqueueSystem(context.Context, jobs.EnqueueRequest) (jobs.Job, error)
}

// NewWebhooks handles webhook POSTs, with credentials independent of browser
// sessions and API tokens.
func NewWebhooks(declarations WebhookDeclarations, secrets WebhookSecrets, queue WebhookJobs) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		ctx := r.Context()
		plugin := r.PathValue("plugin")
		declared, err := declarations.Webhook(ctx, plugin, r.PathValue("name"))
		if err != nil {
			if errors.Is(err, access.ErrNotFound) {
				http.NotFound(w, r)
			} else {
				http.Error(w, "Could not resolve webhook.", http.StatusInternalServerError)
			}
			return
		}

		var header, secretName string
		signed := declared.Verify.HMACSHA256 != nil
		if signed {
			header, secretName = declared.Verify.HMACSHA256.Header, declared.Verify.HMACSHA256.Secret
		} else {
			header, secretName = declared.Verify.Token.Header, declared.Verify.Token.Secret
		}
		value := r.Header.Get(header)
		if value == "" {
			http.Error(w, "Invalid webhook credentials.", http.StatusUnauthorized)
			return
		}
		secret, err := secrets.Value(ctx, secretName, "plugin/"+plugin)
		if err != nil || len(secret) == 0 {
			http.Error(w, "Webhook is unavailable.", http.StatusServiceUnavailable)
			return
		}
		if !signed && !equalWebhookToken(secret, value) {
			http.Error(w, "Invalid webhook credentials.", http.StatusUnauthorized)
			return
		}
		body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, maxWebhookBody))
		if err != nil {
			var sizeErr *http.MaxBytesError
			if errors.As(err, &sizeErr) {
				http.Error(w, "Webhook body is too large.", http.StatusRequestEntityTooLarge)
			} else {
				http.Error(w, "Could not read webhook body.", http.StatusBadRequest)
			}
			return
		}
		if signed && !validWebhookSignature(secret, value, body) {
			http.Error(w, "Invalid webhook credentials.", http.StatusUnauthorized)
			return
		}
		args, err := cborx.Marshal(body)
		if err != nil {
			http.Error(w, "Could not encode webhook body.", http.StatusInternalServerError)
			return
		}
		_, err = queue.EnqueueSystem(ctx, jobs.EnqueueRequest{
			Kind: plugin + "/" + declared.Job, Args: args, Resource: access.PluginURI(plugin),
		})
		if err != nil {
			http.Error(w, "Could not enqueue webhook.", http.StatusServiceUnavailable)
			return
		}
		w.WriteHeader(http.StatusAccepted)
	})
}

func equalWebhookToken(secret []byte, received string) bool {
	want, got := sha256.Sum256(secret), sha256.Sum256([]byte(received))
	return subtle.ConstantTimeCompare(want[:], got[:]) == 1
}

func validWebhookSignature(secret []byte, received string, body []byte) bool {
	signature, err := hex.DecodeString(strings.TrimPrefix(received, "sha256="))
	if err != nil {
		return false
	}
	mac := hmac.New(sha256.New, secret)
	_, _ = mac.Write(body)
	return hmac.Equal(mac.Sum(nil), signature)
}
