package plugins_test

import (
	"bytes"
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/data"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/plugins"
	"github.com/aiviarium/rookery/internal/spaces"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

// installExample packs a small plugin and installs it, as an install from the
// edge does.
func installExample(t *testing.T, installer *plugins.Installer) plugins.Plugin {
	t.Helper()

	var packageBytes bytes.Buffer
	packed, err := plugins.Pack(t.Context(), &packageBytes, writeFixture(t))
	if err != nil {
		t.Fatalf("packing the fixture: %v", err)
	}
	installed, err := installer.Install(t.Context(), bytes.NewReader(packageBytes.Bytes()))
	if err != nil {
		t.Fatalf("installing the fixture: %v", err)
	}
	if installed.SHA256 != packed.SHA256 {
		t.Errorf("the installed hash is %s, the package's is %s", installed.SHA256, packed.SHA256)
	}
	return installed
}

// writeFixture writes a plugin with a collection and a client half, and returns
// its folder.
func writeFixture(t *testing.T) string {
	t.Helper()

	dir := t.TempDir()
	manifest := `id: example
version: 0.1.0
name: Example
collections:
  notes:
    scope: space
    access: { read: view, write: use }
    fields:
      title: { type: text, required: true }
client: web/
contributes:
  views: [{ id: notes, label: Notes, route: /notes }]
`
	if err := os.WriteFile(filepath.Join(dir, "plugin.yaml"), []byte(manifest), 0o600); err != nil {
		t.Fatalf("writing the manifest: %v", err)
	}
	if err := os.MkdirAll(filepath.Join(dir, "web"), 0o750); err != nil {
		t.Fatalf("making the client folder: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, "web", "index.js"), []byte("export default {};\n"), 0o600); err != nil {
		t.Fatalf("writing the client half: %v", err)
	}
	return dir
}

func TestInstallWritesTheRowAndTheFiles(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	stateDir := t.TempDir()
	installer := plugins.NewInstaller(plugins.Options{Store: opened, StateDir: stateDir})

	installed := installExample(t, installer)
	if installed.ID != "example" || installed.Version != "0.1.0" || installed.Name != "Example" {
		t.Fatalf("the installed plugin is %+v", installed)
	}
	if !installed.Enabled {
		t.Error("a plugin is installed disabled")
	}
	if len(installed.Contributes) == 0 {
		t.Error("the fixture's contributions are empty")
	}

	// Its files are under its hash.
	manifest := filepath.Join(stateDir, "plugins", "example", installed.SHA256, "plugin.yaml")
	if _, err := os.Stat(manifest); err != nil {
		t.Fatalf("the package's files are not there: %v", err)
	}

	// Reading it back says the same.
	found, err := installer.Get(t.Context(), "example")
	if err != nil {
		t.Fatalf("reading the plugin: %v", err)
	}
	if found.SHA256 != installed.SHA256 || string(found.Manifest) != string(installed.Manifest) {
		t.Errorf("the plugin read back is %+v", found)
	}

	listed, err := installer.List(t.Context())
	if err != nil {
		t.Fatalf("listing the plugins: %v", err)
	}
	if len(listed) != 1 || listed[0].ID != "example" {
		t.Errorf("the plugins are %+v", listed)
	}

	// The plugin is a resource, so that grants can be made on it.
	if _, err := opened.Queries().GetResource(t.Context(), access.PluginURI("example")); err != nil {
		t.Errorf("the plugin's resource is not there: %v", err)
	}
}

func TestInstallReplacesAPackageWithTheSameCollections(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	installer := plugins.NewInstaller(plugins.Options{Store: opened, StateDir: t.TempDir()})
	first := installExample(t, installer)
	second := installExample(t, installer)
	if second.InstalledAt.IsZero() || second.UpdatedAt.Before(first.InstalledAt) {
		t.Errorf("the second install says nothing about when it happened")
	}
	if second.SHA256 != first.SHA256 {
		t.Errorf("the same package installed twice has two hashes")
	}
}

func TestRemoveTakesTheFilesAndKeepsTheData(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	stateDir := t.TempDir()
	installer := plugins.NewInstaller(plugins.Options{Store: opened, StateDir: stateDir})
	installed := installExample(t, installer)

	if err := installer.Remove(t.Context(), "example", false); err != nil {
		t.Fatalf("removing the plugin: %v", err)
	}
	if _, err := installer.Get(t.Context(), "example"); !errors.Is(err, access.ErrNotFound) {
		t.Fatalf("the removed plugin is still there: %v", err)
	}
	if _, err := os.Stat(filepath.Join(stateDir, "plugins", "example")); !os.IsNotExist(err) {
		t.Errorf("the removed plugin's files are still there: %v", err)
	}
	_ = installed
}

func TestContributionsAndTheShell(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	installer := plugins.NewInstaller(plugins.Options{Store: opened, StateDir: t.TempDir()})
	installed := installExample(t, installer)

	contributions, err := installer.Contributions(t.Context())
	if err != nil {
		t.Fatalf("reading the contributions: %v", err)
	}
	if len(contributions) != 1 || contributions[0].Plugin != "example" || contributions[0].Hash != installed.SHA256 {
		t.Fatalf("the contributions are %+v", contributions)
	}

	if _, ok, err := installer.Shell(t.Context()); err != nil || ok {
		t.Fatalf("the fixture is served as the shell: %v, %v", ok, err)
	}

	// A disabled plugin is in no contribution.
	if _, err := installer.SetEnabled(t.Context(), "example", false); err != nil {
		t.Fatalf("disabling the plugin: %v", err)
	}
	contributions, err = installer.Contributions(t.Context())
	if err != nil {
		t.Fatalf("reading the contributions: %v", err)
	}
	if len(contributions) != 0 {
		t.Errorf("a disabled plugin contributes %+v", contributions)
	}
}

func TestClientHalfAndTheShellAreServedFromThePackage(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	stateDir := t.TempDir()
	installer := plugins.NewInstaller(plugins.Options{Store: opened, StateDir: stateDir})
	installed := installExample(t, installer)

	contents, ok, err := installer.ClientFile(t.Context(), "example", installed.SHA256, "index.js")
	if err != nil || !ok {
		t.Fatalf("reading the client half: %v, %v", ok, err)
	}
	if string(contents) == "" {
		t.Error("the client half is empty")
	}

	// A file that is not there, another hash and a path that leaves the folder
	// are all not found.
	for _, name := range []string{"missing.js", ""} {
		if _, ok, _ := installer.ClientFile(t.Context(), "example", installed.SHA256, name); ok {
			t.Errorf("%q was found", name)
		}
	}
	if _, ok, _ := installer.ClientFile(t.Context(), "example", "0123456789", "index.js"); ok {
		t.Error("a file was found under a hash the plugin does not have")
	}
	if _, ok, _ := installer.ClientFile(t.Context(), "example", installed.SHA256, "../../plugin.yaml"); ok {
		t.Error("a path left the client half's folder")
	}

	// The plugin is not the shell.
	if _, ok, err := installer.ShellIndex(t.Context()); err != nil || ok {
		t.Fatalf("the fixture is served as the shell: %v, %v", ok, err)
	}
}

func TestInstallRefusesAPackageThatIsNotOne(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	installer := plugins.NewInstaller(plugins.Options{Store: opened, StateDir: t.TempDir()})
	if _, err := installer.Install(t.Context(), bytes.NewReader([]byte("not a package"))); !errors.Is(err, plugins.ErrPackage) {
		t.Fatalf("installing rubbish gave %v", err)
	}
}

// authorizer gives everyone manage, for a datastore a test builds.
type authorizer struct{}

// Level is manage on everything.
func (authorizer) Level(context.Context, data.Principal, string) (rookeryv1.Level, error) {
	return rookeryv1.Level_LEVEL_MANAGE, nil
}

// writeDeclaringFixture writes a plugin that declares everything an install
// registers, and returns its folder.
func writeDeclaringFixture(t *testing.T) string {
	t.Helper()

	dir := t.TempDir()
	manifest := `id: notepad
version: 0.1.0
name: Notepad
collections:
  notes:
    scope: space
    access: { read: view, write: use }
    history: true
    fields:
      title: { type: text, required: true }
      done: { type: bool, default: false }
    indexes: [[done]]
    entity: { kind: Note, title: title, text: [title] }
spaceKinds: [{ id: notebook, label: Notebook }]
jobs: [{ kind: reindex, enqueue: { level: manage, resource: space }, timeout: 10m, retries: 3 }]
schedules: [{ kind: reindex, cron: "0 3 * * *" }]
`
	if err := os.WriteFile(filepath.Join(dir, "plugin.yaml"), []byte(manifest), 0o600); err != nil {
		t.Fatalf("writing the manifest: %v", err)
	}
	return dir
}

func TestInstallRegistersWhatThePluginDeclares(t *testing.T) {
	t.Parallel()

	opened := storetest.New(t)
	registry := jobs.NewRegistry()
	schedules := jobs.NewSchedules(opened, registry, jobs.NewService(opened, access.NewChecker(opened), registry, nil, nil), nil)
	kinds := spaces.NewRegistry()
	datastore := data.New(data.Config{Store: opened, Authorizer: authorizer{}})
	installer := plugins.NewInstaller(plugins.Options{
		Store:     opened,
		StateDir:  t.TempDir(),
		Data:      datastore,
		Jobs:      registry,
		Schedules: schedules,
		Spaces:    kinds,
	})

	var packageBytes bytes.Buffer
	if _, err := plugins.Pack(t.Context(), &packageBytes, writeDeclaringFixture(t)); err != nil {
		t.Fatalf("packing the fixture: %v", err)
	}
	if _, err := installer.Install(t.Context(), bytes.NewReader(packageBytes.Bytes())); err != nil {
		t.Fatalf("installing the fixture: %v", err)
	}

	// The collection is declared, and its table is there.
	if !datastore.Declared("notepad/notes") {
		t.Error("the collection is not declared")
	}
	var table *string
	err := opened.Pool().QueryRow(t.Context(), `SELECT to_regclass('p_notepad.notes')::text`).Scan(&table)
	if err != nil || table == nil || *table != "p_notepad.notes" {
		t.Errorf("the collection's table is %v: %v", table, err)
	}

	// Its job kind is the runner's, and its schedule runs it.
	kind, known := registry.Kind("notepad/reindex")
	if !known || kind.Level != access.Manage || kind.Timeout != 10*time.Minute || kind.Retries != 3 {
		t.Errorf("the job kind is %+v, known %v", kind, known)
	}
	found, err := schedules.List(t.Context(), "", 10)
	if err != nil || len(found) != 1 || found[0].Name != "notepad/reindex" || found[0].Kind != "notepad/reindex" {
		t.Fatalf("the schedules are %+v: %v", found, err)
	}

	// Its kind of space is listed after the core's.
	listed := kinds.All()
	if len(listed) != len(spaces.CoreKinds())+1 || listed[len(listed)-1].ID != "notebook" {
		t.Errorf("the kinds of space are %+v", listed)
	}
}
