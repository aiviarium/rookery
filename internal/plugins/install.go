package plugins

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/jackc/pgx/v5"

	"google.golang.org/protobuf/types/known/timestamppb"
	"gopkg.in/yaml.v3"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/data"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/spaces"
	"github.com/aiviarium/rookery/internal/store"
)

// ErrPlugin is the sentinel a refusal of an install, an update or a removal
// wraps.
var ErrPlugin = errors.New("plugins: refused")

// Options are what an installer is made of.
type Options struct {
	Version string
	// Store is the database the plugin rows are in.
	Store *store.Store
	// StateDir is where the packages' files are unpacked:
	// <StateDir>/plugins/<id>/<sha256>/.
	StateDir string
	// Data is the datastore, whose declaration a plugin's collections are, and
	// whose schema a purge drops. Without it a plugin's collections are not
	// declared, which is the case until the datastore's service is wired.
	Data *data.Data
	// Jobs is the job kinds, which a plugin adds to.
	Jobs *jobs.Registry
	// Schedules makes the schedules a plugin starts with.
	Schedules *jobs.Schedules
	// Spaces is the kinds of space a plugin adds.
	Spaces *spaces.Registry
	// Services runs the plugins' services. Without it they are not started.
	Services *Supervisor
	// Logger says what an install did.
	Logger *slog.Logger
}

// Installer installs, updates and removes plugin packages: it records the
// package, unpacks its files, and registers what it declares.
type Installer struct {
	mu        sync.Mutex
	version   string
	store     *store.Store
	stateDir  string
	data      *data.Data
	jobs      *jobs.Registry
	schedules *jobs.Schedules
	spaces    *spaces.Registry
	services  *Supervisor
	logger    *slog.Logger
}

// NewInstaller returns an installer.
func NewInstaller(options Options) *Installer {
	logger := options.Logger
	if logger == nil {
		logger = slog.New(slog.DiscardHandler)
	}
	return &Installer{
		store:     options.Store,
		version:   options.Version,
		stateDir:  options.StateDir,
		data:      options.Data,
		jobs:      options.Jobs,
		schedules: options.Schedules,
		spaces:    options.Spaces,
		services:  options.Services,
		logger:    logger,
	}
}

// SetServices makes the installer start and stop the plugins' services. It is
// set after both exist, since the supervisor reads its plugins back through the
// installer.
func (i *Installer) SetServices(services *Supervisor) { i.services = services }

// Plugin is an installed package.
type Plugin struct {
	ID      string
	Name    string
	Version string
	SHA256  string
	Enabled bool
	// Manifest is the plugin.yaml the package was installed with.
	Manifest []byte
	// Shell says its client half is the UI served at /.
	Shell bool
	// Contributes is what its client half adds, as JSON.
	Contributes []byte
	InstalledAt time.Time
	UpdatedAt   time.Time
}

// Message is a plugin as the edge API has it.
func (p Plugin) Message() *rookeryv1.Plugin {
	out := &rookeryv1.Plugin{
		Id:              p.ID,
		Name:            p.Name,
		Version:         p.Version,
		Sha256:          p.SHA256,
		Enabled:         p.Enabled,
		Manifest:        p.Manifest,
		ContributesJson: p.Contributes,
		Shell:           p.Shell,
	}
	if !p.InstalledAt.IsZero() {
		out.InstalledAt = timestamp(p.InstalledAt)
	}
	if !p.UpdatedAt.IsZero() {
		out.UpdatedAt = timestamp(p.UpdatedAt)
	}
	return out
}

// Dir is where a plugin's files are.
func (i *Installer) Dir(id, sha string) string {
	return filepath.Join(i.stateDir, "plugins", id, sha)
}

// Install reads a package, records it and registers what it declares. A package
// whose id is installed already replaces it, as long as its collection
// declarations are the same.
func (i *Installer) Install(ctx context.Context, packageReader io.Reader) (Plugin, error) {
	return i.install(ctx, packageReader, false)
}

func (i *Installer) install(ctx context.Context, packageReader io.Reader, requireNew bool) (Plugin, error) {
	staging, err := os.MkdirTemp(i.stagingDir(), "install-")
	if err != nil {
		return Plugin{}, fmt.Errorf("plugins: making a place for the package: %w", err)
	}
	defer func() { _ = os.RemoveAll(staging) }()

	unpacked, err := Unpack(ctx, packageReader, staging)
	if err != nil {
		return Plugin{}, err
	}
	manifest := unpacked.Manifest

	i.mu.Lock()
	defer i.mu.Unlock()
	if err := i.checkRequirements(ctx, manifest); err != nil {
		return Plugin{}, err
	}

	previous, err := i.Get(ctx, manifest.ID)
	switch {
	case errors.Is(err, access.ErrNotFound):
		// A plugin that is not there yet declares its collections, job kinds
		// and kinds of space before anything is made.
		if err := i.declare(ctx, manifest); err != nil {
			return Plugin{}, err
		}
	case err != nil:
		return Plugin{}, err
	default:
		if requireNew {
			return Plugin{}, fmt.Errorf("%w: %s is installed", access.ErrConflict, manifest.ID)
		}
		if err := sameCollections(previous, manifest); err != nil {
			return Plugin{}, err
		}
	}

	// The package's files go where its hash says.
	destination := i.Dir(manifest.ID, unpacked.SHA256)
	if err := os.MkdirAll(filepath.Dir(destination), 0o750); err != nil {
		return Plugin{}, fmt.Errorf("plugins: making the plugins folder: %w", err)
	}
	if err := os.RemoveAll(destination); err != nil {
		return Plugin{}, fmt.Errorf("plugins: clearing %s: %w", destination, err)
	}
	if err := os.Rename(staging, destination); err != nil {
		return Plugin{}, fmt.Errorf("plugins: moving the package into place: %w", err)
	}
	// The old version's files go once the new ones are in.
	if previous.ID != "" && previous.SHA256 != unpacked.SHA256 {
		_ = os.RemoveAll(i.Dir(previous.ID, previous.SHA256))
	}

	manifestBytes, err := os.ReadFile(filepath.Join(destination, "plugin.yaml")) //nolint:gosec // the path is the package the installer just unpacked
	if err != nil {
		return Plugin{}, fmt.Errorf("plugins: reading the installed manifest: %w", err)
	}
	record, err := i.store.Queries().UpsertPlugin(ctx, store.UpsertPluginParams{
		ID:       manifest.ID,
		Name:     manifest.Name,
		Version:  manifest.Version,
		Sha256:   unpacked.SHA256,
		Manifest: manifestBytes,
		Enabled:  true,
	})
	if err != nil {
		return Plugin{}, fmt.Errorf("plugins: recording %s: %w", manifest.ID, err)
	}

	// The plugin is a resource of its own, so grants can be made on it.
	if err := i.store.Queries().UpsertResource(ctx, store.UpsertResourceParams{Uri: access.PluginURI(manifest.ID)}); err != nil {
		return Plugin{}, fmt.Errorf("plugins: recording the plugin's resource: %w", err)
	}

	i.declareAtRuntime(ctx, manifest)
	i.logger.Info("installed a plugin", slog.String("plugin", manifest.ID), slog.String("version", manifest.Version))
	plugin, err := pluginOf(record)
	if err != nil {
		return Plugin{}, err
	}
	if i.services != nil && manifest.Service != nil && manifest.Service.Start != StartLazy {
		if err := i.services.Start(context.WithoutCancel(ctx), plugin); err != nil {
			i.logger.Error("starting a plugin's service", slog.String("plugin", plugin.ID), slog.String("error", err.Error()))
		}
	}
	return plugin, nil
}

// declare adds a plugin's declarations to the packages that own them, and makes
// its tables: a declaration that is refused leaves no table behind.
func (i *Installer) declare(ctx context.Context, manifest *Manifest) error {
	if i.data != nil {
		collections := make([]*data.Collection, 0, len(manifest.Collections))
		for name, declaration := range manifest.Collections {
			collection, err := collectionOf(manifest.ID, name, declaration)
			if err != nil {
				return err
			}
			collections = append(collections, collection)
		}
		if err := i.data.Declare(collections...); err != nil {
			return fmt.Errorf("plugins: declaring the collections of %s: %w", manifest.ID, err)
		}
		if err := i.data.Create(ctx, manifest.ID); err != nil {
			return err
		}
	}

	if i.jobs != nil {
		kinds := make([]jobs.Kind, 0, len(manifest.Jobs))
		for _, declared := range manifest.Jobs {
			kind, err := kindOf(manifest.ID, declared)
			if err != nil {
				return err
			}
			kinds = append(kinds, kind)
		}
		if err := i.jobs.RegisterKinds(kinds...); err != nil {
			return fmt.Errorf("plugins: registering the jobs of %s: %w", manifest.ID, err)
		}
	}

	if i.spaces != nil && len(manifest.SpaceKinds) > 0 {
		kinds := make([]spaces.Kind, 0, len(manifest.SpaceKinds))
		for _, declared := range manifest.SpaceKinds {
			kinds = append(kinds, spaceKindOf(manifest.ID, declared))
		}
		if err := i.spaces.Declare(kinds...); err != nil {
			return fmt.Errorf("plugins: declaring the kinds of space of %s: %w", manifest.ID, err)
		}
	}
	return nil
}

// declareAtRuntime adds what an install starts besides the declarations: the
// schedules the plugin runs on.
func (i *Installer) declareAtRuntime(ctx context.Context, manifest *Manifest) {
	if i.schedules == nil {
		return
	}
	for index, declared := range manifest.Schedules {
		name := manifest.ID + "/" + declared.Kind
		if index > 0 {
			name = fmt.Sprintf("%s/%d", name, index)
		}
		kind, err := kindOf(manifest.ID, Job{Kind: declared.Kind})
		if err != nil {
			continue
		}
		if _, err := i.schedules.Create(context.WithoutCancel(ctx), jobs.ScheduleRequest{
			Name: name,
			Kind: kind.Name,
			Cron: declared.Cron,
		}); err != nil {
			// An update keeps the schedule it made the first time.
			i.logger.Warn("making a plugin's schedule",
				slog.String("plugin", manifest.ID), slog.String("schedule", name), slog.String("error", err.Error()))
		}
	}
}

// Get returns an installed plugin.
func (i *Installer) Get(ctx context.Context, id string) (Plugin, error) {
	record, err := i.store.Queries().GetPlugin(ctx, id)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return Plugin{}, fmt.Errorf("%w: no plugin %s", access.ErrNotFound, id)
	case err != nil:
		return Plugin{}, fmt.Errorf("plugins: reading %s: %w", id, err)
	}
	return pluginOf(record)
}

// List returns the installed plugins, by id.
func (i *Installer) List(ctx context.Context) ([]Plugin, error) {
	records, err := i.store.Queries().ListPlugins(ctx)
	if err != nil {
		return nil, fmt.Errorf("plugins: listing the plugins: %w", err)
	}
	out := make([]Plugin, 0, len(records))
	for _, record := range records {
		plugin, err := pluginOf(record)
		if err != nil {
			return nil, err
		}
		out = append(out, plugin)
	}
	return out, nil
}

// Remove takes a plugin away. It refuses while another plugin needs it, and
// keeps its data unless purge is set.
func (i *Installer) Remove(ctx context.Context, id string, purge bool) error {
	i.mu.Lock()
	defer i.mu.Unlock()
	plugin, err := i.Get(ctx, id)
	if err != nil {
		return err
	}
	plugins, err := i.List(ctx)
	if err != nil {
		return err
	}
	for _, other := range plugins {
		if other.ID == id {
			continue
		}
		var manifest Manifest
		if err := yamlUnmarshal(other.Manifest, &manifest); err != nil {
			continue
		}
		if _, needed := manifest.Dependencies[id]; needed {
			return fmt.Errorf("%w: %s needs %s", access.ErrConflict, other.ID, id)
		}
	}

	// The process goes before its files do.
	if i.services != nil {
		i.services.Stop(id)
	}
	if purge && i.data != nil {
		if err := i.data.Drop(ctx, id); err != nil {
			return err
		}
	}
	if _, err := i.store.Queries().DeletePlugin(ctx, id); err != nil {
		return fmt.Errorf("plugins: deleting %s: %w", id, err)
	}
	if _, err := i.store.Queries().DeleteSchedulesNamed(ctx, id+"/%"); err != nil {
		return fmt.Errorf("plugins: deleting the schedules of %s: %w", id, err)
	}
	if err := os.RemoveAll(filepath.Join(i.stateDir, "plugins", id)); err != nil {
		return fmt.Errorf("plugins: removing the files of %s: %w", id, err)
	}
	_ = plugin
	return nil
}

// SetEnabled turns a plugin on or off.
func (i *Installer) SetEnabled(ctx context.Context, id string, enabled bool) (Plugin, error) {
	i.mu.Lock()
	defer i.mu.Unlock()
	if i.services != nil && !enabled {
		i.services.Stop(id)
	}
	record, err := i.store.Queries().SetPluginEnabled(ctx, store.SetPluginEnabledParams{ID: id, Enabled: enabled})
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return Plugin{}, fmt.Errorf("%w: no plugin %s", access.ErrNotFound, id)
	case err != nil:
		return Plugin{}, fmt.Errorf("plugins: changing %s: %w", id, err)
	}
	plugin, err := pluginOf(record)
	if err != nil {
		return Plugin{}, err
	}
	if i.services != nil && enabled {
		if manifest, err := pluginManifest(plugin); err == nil && manifest.Service != nil && manifest.Service.Start != StartLazy {
			if err := i.services.Start(context.WithoutCancel(ctx), plugin); err != nil {
				i.logger.Error("starting a plugin's service", slog.String("plugin", id), slog.String("error", err.Error()))
			}
		}
	}
	return plugin, nil
}

// Contribution is what the shell needs from one plugin that has a client half:
// its contributes, and the hash its files are served under.
type Contribution struct {
	Plugin      string
	Hash        string
	Shell       bool
	Contributes []byte
}

// Contributions returns what the enabled plugins with a client half add.
func (i *Installer) Contributions(ctx context.Context) ([]Contribution, error) {
	records, err := i.store.Queries().ListEnabledPlugins(ctx)
	if err != nil {
		return nil, fmt.Errorf("plugins: listing the enabled plugins: %w", err)
	}
	out := make([]Contribution, 0, len(records))
	for _, record := range records {
		plugin, err := pluginOf(record)
		if err != nil {
			return nil, err
		}
		if len(plugin.Contributes) == 0 && !plugin.Shell {
			continue
		}
		out = append(out, Contribution{
			Plugin:      plugin.ID,
			Hash:        plugin.SHA256,
			Shell:       plugin.Shell,
			Contributes: plugin.Contributes,
		})
	}
	return out, nil
}

// Shell returns the enabled plugin whose client half is the UI, if any.
func (i *Installer) Shell(ctx context.Context) (Plugin, bool, error) {
	plugins, err := i.List(ctx)
	if err != nil {
		return Plugin{}, false, err
	}
	for _, plugin := range plugins {
		if plugin.Enabled && plugin.Shell {
			return plugin, true, nil
		}
	}
	return Plugin{}, false, nil
}

// stagingDir is where packages are unpacked before they are put in place.
func (i *Installer) stagingDir() string {
	dir := filepath.Join(i.stateDir, "plugins", ".staging")
	if err := os.MkdirAll(dir, 0o750); err != nil {
		return os.TempDir() //nolint:forbidigo // the fallback is only reached when the state dir cannot be made
	}
	return dir
}

// pluginOf is an installed plugin from its row.
func pluginOf(record store.RookeryPlugins) (Plugin, error) {
	plugin := Plugin{
		ID:       record.ID,
		Name:     record.Name,
		Version:  record.Version,
		SHA256:   record.Sha256,
		Enabled:  record.Enabled,
		Manifest: record.Manifest,
	}
	if !record.InstalledAt.IsZero() {
		plugin.InstalledAt = record.InstalledAt
	}
	if !record.UpdatedAt.IsZero() {
		plugin.UpdatedAt = record.UpdatedAt
	}

	var manifest Manifest
	if err := yamlUnmarshal(record.Manifest, &manifest); err == nil {
		plugin.Shell = manifest.Shell
		if manifest.Contributes != nil {
			if encoded, err := json.Marshal(manifest.Contributes); err == nil {
				plugin.Contributes = encoded
			}
		}
	}
	return plugin, nil
}

// sameCollections refuses an update that changes what the plugin's tables are.
func sameCollections(previous Plugin, manifest *Manifest) error {
	var old Manifest
	if err := yamlUnmarshal(previous.Manifest, &old); err != nil {
		return fmt.Errorf("plugins: reading the manifest of %s: %w", previous.ID, err)
	}
	was, err := json.Marshal(old.Collections)
	if err != nil {
		return fmt.Errorf("plugins: reading the collections of %s: %w", previous.ID, err)
	}
	now, err := json.Marshal(manifest.Collections)
	if err != nil {
		return fmt.Errorf("plugins: reading the collections of %s: %w", previous.ID, err)
	}
	if string(was) != string(now) {
		return fmt.Errorf("%w: %s changes its collections, which this release cannot do", access.ErrConflict, manifest.ID)
	}
	return nil
}

// timestamp is a time as the edge API has it.
func timestamp(at time.Time) *timestamppb.Timestamp { return timestamppb.New(at) }

// yamlUnmarshal reads a manifest, which is YAML.
func yamlUnmarshal(data []byte, value any) error {
	if err := yaml.Unmarshal(data, value); err != nil {
		return fmt.Errorf("plugins: reading a manifest: %w", err)
	}
	return nil
}

// ClientFile is a file of a plugin's client half, by the hash its files are
// served under. It refuses a name that leaves the folder.
func (i *Installer) ClientFile(ctx context.Context, pluginID, hash, name string) ([]byte, bool, error) {
	plugin, err := i.Get(ctx, pluginID)
	if err != nil {
		if errors.Is(err, access.ErrNotFound) {
			return nil, false, nil
		}
		return nil, false, err
	}
	if plugin.SHA256 != hash || !plugin.Enabled {
		return nil, false, nil
	}
	client := i.clientFolder(plugin)
	if client == "" {
		return nil, false, nil
	}
	file, ok := fileIn(i.Dir(pluginID, hash), filepath.Join(client, name))
	if !ok {
		return nil, false, nil
	}
	contents, err := os.ReadFile(file) //nolint:gosec // fileIn has refused a path that leaves the plugin's folder
	if err != nil {
		if os.IsNotExist(err) {
			return nil, false, nil
		}
		return nil, false, err
	}
	return contents, true, nil
}

// ShellIndex is the installed shell's index.html, when a plugin is the shell.
func (i *Installer) ShellIndex(ctx context.Context) ([]byte, bool, error) {
	plugin, ok, err := i.Shell(ctx)
	if err != nil || !ok {
		return nil, false, err
	}
	client := i.clientFolder(plugin)
	if client == "" {
		return nil, false, nil
	}
	file, ok := fileIn(i.Dir(plugin.ID, plugin.SHA256), filepath.Join(client, "index.html"))
	if !ok {
		return nil, false, nil
	}
	contents, err := os.ReadFile(file) //nolint:gosec // fileIn has refused a path that leaves the plugin's folder
	if err != nil {
		if os.IsNotExist(err) {
			return nil, false, nil
		}
		return nil, false, err
	}
	return contents, true, nil
}

// clientFolder is the folder of a plugin's client half, empty when it has none.
func (i *Installer) clientFolder(plugin Plugin) string {
	var manifest Manifest
	if err := yamlUnmarshal(plugin.Manifest, &manifest); err != nil {
		return ""
	}
	return strings.Trim(strings.TrimSpace(manifest.Client), "/")
}

// fileIn joins a name under a folder, and refuses one that leaves it.
func fileIn(folder, name string) (string, bool) {
	file := filepath.Join(folder, filepath.FromSlash(name))
	relative, err := filepath.Rel(folder, file)
	if err != nil || relative == ".." || strings.HasPrefix(relative, ".."+string(filepath.Separator)) || filepath.IsAbs(relative) {
		return "", false
	}
	return file, true
}

// Access is what a declared procedure needs, from the installed manifest. It
// reports whether the plugin declares the procedure at all.
func (i *Installer) Access(ctx context.Context, id, procedure string) (Level, Resource, bool, error) {
	plugin, err := i.Get(ctx, id)
	if err != nil {
		return "", "", false, err
	}
	manifest, err := pluginManifest(plugin)
	if err != nil {
		return "", "", false, err
	}
	if manifest.Service == nil {
		return "", "", false, nil
	}
	for _, declared := range manifest.Service.RPC {
		if declared.Procedure != procedure {
			continue
		}
		return declared.Access.Level, declared.Access.Resource, true, nil
	}
	return "", "", false, nil
}

func (i *Installer) checkRequirements(ctx context.Context, manifest *Manifest) error {
	if manifest.Requires != "" && i.version != "" && i.version != "dev" {
		if err := checkRequirement(i.version, manifest.Requires); err != nil {
			return fmt.Errorf("plugin %s needs Rookery %s: %w", manifest.ID, manifest.Requires, err)
		}
	}
	for id, requirement := range manifest.Dependencies {
		dependency, err := i.Get(ctx, id)
		if err != nil {
			return fmt.Errorf("plugin %s depends on %s: %w", manifest.ID, id, err)
		}
		if !dependency.Enabled {
			return fmt.Errorf("%w: plugin %s depends on disabled plugin %s", access.ErrConflict, manifest.ID, id)
		}
		if err := checkRequirement(dependency.Version, requirement); err != nil {
			return fmt.Errorf("plugin %s needs %s %s: %w", manifest.ID, id, requirement, err)
		}
	}
	return nil
}

func checkRequirement(version, constraint string) error {
	v, err := ParseVersion(version)
	if err != nil {
		return err
	}
	requirement, err := ParseRequirement(constraint)
	if err != nil {
		return err
	}
	if !requirement.Allows(v) {
		return fmt.Errorf("%w: version %s does not satisfy %s", access.ErrInvalid, version, constraint)
	}
	return nil
}

// Webhook finds a declaration belonging to an enabled plugin.
func (i *Installer) Webhook(ctx context.Context, pluginID, name string) (Webhook, error) {
	plugin, err := i.Get(ctx, pluginID)
	if err != nil {
		return Webhook{}, err
	}
	if plugin.Enabled {
		manifest, err := pluginManifest(plugin)
		if err != nil {
			return Webhook{}, err
		}
		for _, webhook := range manifest.Webhooks {
			if webhook.Name == name {
				return webhook, nil
			}
		}
	}
	return Webhook{}, access.ErrNotFound
}
