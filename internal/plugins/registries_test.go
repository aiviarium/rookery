package plugins

import (
	"crypto/ed25519"
	"crypto/rand"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

func TestInstallFromSignedRegistry(t *testing.T) {
	t.Parallel()
	opened := storetest.New(t)
	installer := NewInstaller(Options{Store: opened, StateDir: t.TempDir(), Version: "1.0.0"})
	folder := t.TempDir()
	if err := os.WriteFile(filepath.Join(folder, "plugin.yaml"), []byte("id: notes\nname: Notes\nversion: 1.0.0\nrequires: '>=1.0.0'\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	path := filepath.Join(dir, "notes.tar.gz")
	file, err := os.Create(path) //nolint:gosec // test archive in a private directory
	if err != nil {
		t.Fatal(err)
	}
	_, err = Pack(t.Context(), file, folder)
	closeErr := file.Close()
	if err != nil || closeErr != nil {
		t.Fatalf("pack: %v, close: %v", err, closeErr)
	}
	public, private, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	index, signature, err := IndexDir(t.Context(), dir, private)
	if err != nil {
		t.Fatal(err)
	}
	for name, content := range map[string][]byte{"index.json": index, "index.json.sig": signature} {
		if err := os.WriteFile(filepath.Join(dir, name), content, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	server := httptest.NewTLSServer(http.FileServer(http.Dir(dir)))
	defer server.Close()
	service := NewService(installer)
	service.registries.client = server.Client()
	if _, err := service.registries.Add(t.Context(), connect.NewRequest(&rookeryv1.RegistryServiceAddRequest{
		Registry: &rookeryv1.Registry{Name: "test", Url: server.URL, PublicKey: public},
	})); err != nil {
		t.Fatal(err)
	}
	request := connect.NewRequest(&rookeryv1.PluginServiceInstallFromRegistryRequest{Reference: "notes@1.0.0"})
	installed, err := service.InstallFromRegistry(t.Context(), request)
	if err != nil || installed.Msg.GetPlugin().GetId() != "notes" {
		t.Fatalf("install from registry: %v, %v", installed, err)
	}
	if err := installer.Remove(t.Context(), "notes", false); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte("tampered"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := service.InstallFromRegistry(t.Context(), request); connect.CodeOf(err) != connect.CodeInvalidArgument {
		t.Fatalf("tampered package accepted: %v", err)
	}
}
