package edge_test

import (
	"bytes"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"

	"connectrpc.com/connect"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/config"
	"github.com/aiviarium/rookery/internal/edge"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/plugins"
	"github.com/aiviarium/rookery/internal/seal"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

func TestVerifiedWebhookEnqueuesBody(t *testing.T) {
	t.Parallel()
	opened := storetest.New(t)
	ctx := t.Context()
	key, err := seal.Load("", filepath.Join(t.TempDir(), "key"), true)
	if err != nil {
		t.Fatal(err)
	}
	ring, err := seal.NewRing(key.ID(), key)
	if err != nil {
		t.Fatal(err)
	}
	secrets := config.NewSecrets(opened, ring)
	secret := []byte("fixture-only-secret")
	if _, err := secrets.Set(ctx, "push_token", "plugin/notes", secret); err != nil {
		t.Fatal(err)
	}
	kinds := jobs.NewRegistry()
	queue := jobs.NewService(opened, access.NewChecker(opened), kinds, nil, nil)
	installer := plugins.NewInstaller(plugins.Options{Store: opened, StateDir: t.TempDir(), Jobs: kinds})
	dir := t.TempDir()
	manifest := []byte(`id: notes
name: Notes
version: 1.0.0
secrets: [push_token]
jobs: [{kind: push, enqueue: {level: manage, resource: plugin}}]
webhooks:
  - name: signed
    verify: {hmac-sha256: {header: X-Signature, secret: push_token}}
    job: push
  - name: token
    verify: {token: {header: Authorization, secret: push_token}}
    job: push
`)
	if err := os.WriteFile(filepath.Join(dir, "plugin.yaml"), manifest, 0o600); err != nil {
		t.Fatal(err)
	}
	var archive bytes.Buffer
	if _, err := plugins.Pack(ctx, &archive, dir); err != nil {
		t.Fatal(err)
	}
	if _, err := installer.Install(ctx, &archive); err != nil {
		t.Fatal(err)
	}
	handler := edge.New(edge.Deps{
		Webhooks:         edge.NewWebhooks(installer, secrets, queue),
		RegisterServices: func(*http.ServeMux, []connect.HandlerOption) {},
	}).Handler()
	body := []byte(`{"ref":"refs/heads/main"}`)
	mac := hmac.New(sha256.New, secret)
	_, _ = mac.Write(body)
	for _, tc := range []struct {
		name, header, credential string
		status                   int
	}{
		{"signed", "X-Signature", "sha256=bad", http.StatusUnauthorized},
		{"signed", "X-Signature", "sha256=" + hex.EncodeToString(mac.Sum(nil)), http.StatusAccepted},
		{"token", "Authorization", string(secret), http.StatusAccepted},
	} {
		request := httptest.NewRequestWithContext(ctx, http.MethodPost, "/hooks/notes/"+tc.name, bytes.NewReader(body))
		request.Header.Set(tc.header, tc.credential)
		recorder := httptest.NewRecorder()
		handler.ServeHTTP(recorder, request)
		if recorder.Code != tc.status {
			t.Fatalf("%s: %d (%s), want %d", tc.name, recorder.Code, recorder.Body.String(), tc.status)
		}
	}
	listed, err := queue.List(access.WithPrincipal(ctx, access.LocalAdmin()), jobs.Filter{})
	if err != nil || len(listed) != 2 {
		t.Fatalf("expected two jobs: %v, %v", listed, err)
	}
	for _, job := range listed {
		var decoded []byte
		if err := cborx.Unmarshal(job.Args, &decoded); err != nil || !bytes.Equal(decoded, body) {
			t.Fatalf("wrong job body: %q, %v", decoded, err)
		}
	}
	if _, err := installer.SetEnabled(ctx, "notes", false); err != nil {
		t.Fatal(err)
	}
	request := httptest.NewRequestWithContext(ctx, http.MethodPost, "/hooks/notes/token", bytes.NewReader(body))
	request.Header.Set("Authorization", string(secret))
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, request)
	if recorder.Code != http.StatusNotFound {
		t.Fatalf("disabled webhook returned %d", recorder.Code)
	}
}
