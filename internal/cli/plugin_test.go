package cli

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// TestPluginCheckAndPackDoNotNeedAServer checks the two plugin commands that
// work on a folder.
func TestPluginCheckAndPackDoNotNeedAServer(t *testing.T) {
	dir := t.TempDir()
	manifest := `id: notes
version: 1.2.3
name: Notes
client: web/
`
	if err := os.WriteFile(filepath.Join(dir, "plugin.yaml"), []byte(manifest), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Join(dir, "web"), 0o750); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "web", "index.js"), []byte("export default {};\n"), 0o600); err != nil {
		t.Fatal(err)
	}

	root := New("test")
	root.SetArgs([]string{"plugin", "check", dir})
	out := &strings.Builder{}
	root.SetOut(out)
	if err := root.Execute(); err != nil {
		t.Fatalf("check: %v", err)
	}
	if !strings.HasPrefix(out.String(), "notes 1.2.3") {
		t.Fatalf("check printed %q", out.String())
	}

	packed := filepath.Join(t.TempDir(), "notes.tar.gz")
	root = New("test")
	root.SetArgs([]string{"plugin", "pack", dir, "--output", packed})
	out.Reset()
	root.SetOut(out)
	if err := root.Execute(); err != nil {
		t.Fatalf("pack: %v", err)
	}
	info, err := os.Stat(packed)
	if err != nil {
		t.Fatalf("the package is not there: %v", err)
	}
	if info.Size() == 0 || !strings.Contains(out.String(), packed) {
		t.Fatalf("pack wrote %d bytes and said %q", info.Size(), out.String())
	}
}
