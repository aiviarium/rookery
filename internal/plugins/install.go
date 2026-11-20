package plugins

import (
	"bytes"
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
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/config"
	"github.com/aiviarium/rookery/internal/data"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/profile"
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
	Data        *data.Data
	SQL         *data.Data
	Settings    *config.Settings
	Connections *config.Connections
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
	mu          sync.Mutex
	version     string
	store       *store.Store
	stateDir    string
	data        *data.Data
	sql         *data.Data
	settings    *config.Settings
	connections *config.Connections
	jobs        *jobs.Registry
	schedules   *jobs.Schedules
	spaces      *spaces.Registry
	services    *Supervisor
	logger      *slog.Logger
}

// NewInstaller returns an installer.
func NewInstaller(options Options) *Installer {
	logger := options.Logger
	if logger == nil {
		logger = slog.New(slog.DiscardHandler)
	}
	return &Installer{
		store:       options.Store,
		version:     options.Version,
		stateDir:    options.StateDir,
		data:        options.Data,
		sql:         options.SQL,
		settings:    options.Settings,
		connections: options.Connections,
		jobs:        options.Jobs,
		schedules:   options.Schedules,
		spaces:      options.Spaces,
		services:    options.Services,
		logger:      logger,
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

// Install records a package, replacing a compatible older version if present.
func (i *Installer) Install(ctx context.Context, packageReader io.Reader) (Plugin, error) {
	return i.install(ctx, packageReader, installOptions{})
}

type installOptions struct {
	requireNew      bool
	requireExisting bool
	expectedID      string
	resetData       bool
}

func (i *Installer) install(ctx context.Context, packageReader io.Reader, opts installOptions) (Plugin, error) {
	staging, err := os.MkdirTemp(i.stagingDir(), "install-")
	if err != nil {
		return Plugin{}, fmt.Errorf("plugins: staging package: %w", err)
	}
	defer func() { _ = os.RemoveAll(staging) }()
	unpacked, err := Unpack(ctx, packageReader, staging)
	if err != nil {
		return Plugin{}, err
	}
	manifest := unpacked.Manifest
	if opts.expectedID != "" && manifest.ID != opts.expectedID {
		return Plugin{}, fmt.Errorf("%w: package id is %s, expected %s", access.ErrInvalid, manifest.ID, opts.expectedID)
	}
	contents, err := os.ReadFile(filepath.Join(staging, FileName)) //nolint:gosec // installed manifest
	if err != nil {
		return Plugin{}, err
	}

	i.mu.Lock()
	defer i.mu.Unlock()
	if err := i.checkRequirements(ctx, manifest); err != nil {
		return Plugin{}, err
	}
	previous, err := i.Get(ctx, manifest.ID)
	if err != nil && !errors.Is(err, access.ErrNotFound) {
		return Plugin{}, err
	}
	if previous.ID != "" && opts.requireNew {
		return Plugin{}, fmt.Errorf("%w: %s is installed", access.ErrConflict, manifest.ID)
	}
	if previous.ID == "" && opts.requireExisting {
		return Plugin{}, fmt.Errorf("%w: %s is not installed", access.ErrNotFound, manifest.ID)
	}
	if err := i.checkReverseDependencies(ctx, manifest); err != nil {
		return Plugin{}, err
	}

	prepared, err := i.prepare(manifest)
	if err != nil {
		return Plugin{}, err
	}
	tx, err := i.store.Pool().Begin(ctx)
	if err != nil {
		return Plugin{}, fmt.Errorf("plugins: beginning install: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()
	q := i.store.Queries().WithTx(tx)
	retained, err := q.GetPluginDeclaration(ctx, manifest.ID)
	if err != nil && !errors.Is(err, pgx.ErrNoRows) {
		return Plugin{}, err
	}
	if retained != nil && !opts.resetData {
		if err := sameCollections(Plugin{ID: manifest.ID, Manifest: retained}, manifest); err != nil {
			return Plugin{}, err
		}
	}
	if i.data != nil {
		if opts.resetData {
			if err := data.DropIn(ctx, tx, manifest.ID); err != nil {
				return Plugin{}, err
			}
		}
		if retained == nil || opts.resetData {
			if err := data.CreateIn(ctx, tx, manifest.ID, prepared.collections); err != nil {
				return Plugin{}, err
			}
		}
	}
	if manifest.Database == DatabaseSQL && i.sql != nil {
		if err := i.sql.EnsureSQLRoleIn(ctx, tx, manifest.ID); err != nil {
			return Plugin{}, err
		}
	}
	enabled := previous.ID == "" || previous.Enabled
	record, err := q.UpsertPlugin(ctx, store.UpsertPluginParams{
		ID: manifest.ID, Name: manifest.Name, Version: manifest.Version, Sha256: unpacked.SHA256,
		Manifest: contents, Enabled: enabled,
	})
	if err != nil {
		return Plugin{}, fmt.Errorf("plugins: recording %s: %w", manifest.ID, err)
	}
	if err := q.PutPluginArchive(ctx, store.PutPluginArchiveParams{Plugin: manifest.ID, Archive: unpacked.archive}); err != nil {
		return Plugin{}, err
	}
	if err := q.PutPluginDeclaration(ctx, store.PutPluginDeclarationParams{Plugin: manifest.ID, Manifest: contents}); err != nil {
		return Plugin{}, err
	}
	if err := q.UpsertResource(ctx, store.UpsertResourceParams{Uri: access.PluginURI(manifest.ID)}); err != nil {
		return Plugin{}, err
	}
	destination := i.Dir(manifest.ID, unpacked.SHA256)
	added, err := movePackage(staging, destination)
	if err != nil {
		return Plugin{}, err
	}
	if err := tx.Commit(ctx); err != nil {
		if added {
			_ = os.RemoveAll(destination)
		}
		return Plugin{}, fmt.Errorf("plugins: committing install: %w", err)
	}

	plugin, err := pluginOf(record)
	if err != nil {
		return Plugin{}, err
	}
	if err := i.activate(manifest, prepared, enabled); err != nil {
		return Plugin{}, err
	}
	if enabled {
		i.declareAtRuntime(ctx, manifest)
	}
	if i.services != nil {
		i.services.Stop(plugin.ID)
		if enabled && manifest.Service != nil && manifest.Service.Start != StartLazy {
			if err := i.services.Start(context.WithoutCancel(ctx), plugin); err != nil {
				i.logger.Error("starting plugin service", "plugin", plugin.ID, "error", err.Error())
			}
		}
	}
	if previous.ID != "" && previous.SHA256 != plugin.SHA256 {
		_ = os.RemoveAll(i.Dir(previous.ID, previous.SHA256))
	}
	return plugin, nil
}

func movePackage(staging, destination string) (bool, error) {
	_, err := os.Stat(destination)
	if err == nil {
		return false, nil
	}
	if !errors.Is(err, os.ErrNotExist) {
		return false, err
	}
	if err := os.MkdirAll(filepath.Dir(destination), 0o750); err != nil {
		return false, err
	}
	if err := os.Rename(staging, destination); err != nil {
		return false, fmt.Errorf("plugins: moving package into place: %w", err)
	}
	return true, nil
}

type declarations struct {
	collections []*data.Collection
	kinds       []jobs.Kind
	spaces      []spaces.Kind
}

func (i *Installer) prepare(manifest *Manifest) (declarations, error) {
	var out declarations
	for name, declaration := range manifest.Collections {
		collection, err := collectionOf(manifest.ID, name, declaration)
		if err != nil {
			return declarations{}, err
		}
		if err := collection.Validate(); err != nil {
			return declarations{}, err
		}
		out.collections = append(out.collections, collection)
	}
	for _, declared := range manifest.Jobs {
		kind, err := kindOf(manifest.ID, declared)
		if err != nil {
			return declarations{}, err
		}
		out.kinds = append(out.kinds, kind)
	}
	for _, kind := range manifest.SpaceKinds {
		out.spaces = append(out.spaces, spaceKindOf(manifest.ID, kind))
	}
	if i.spaces != nil {
		if err := i.spaces.CheckReplace(manifest.ID, out.spaces...); err != nil {
			return declarations{}, err
		}
	}
	return out, nil
}

func (i *Installer) activate(manifest *Manifest, prepared declarations, enabled bool) error {
	if !enabled {
		i.clearDeclarations(manifest.ID)
		return nil
	}
	if i.data != nil {
		i.data.Forget(manifest.ID)
		if err := i.data.Declare(prepared.collections...); err != nil {
			return err
		}
	}
	if i.jobs != nil {
		i.jobs.ReplacePluginKinds(manifest.ID, prepared.kinds)
	}
	if i.spaces != nil {
		if err := i.spaces.Replace(manifest.ID, prepared.spaces...); err != nil {
			return err
		}
	}
	i.applySettings(manifest.ID, manifest)
	i.applyConnections(manifest.ID, manifest)
	return nil
}

func (i *Installer) clearDeclarations(plugin string) {
	if i.data != nil {
		i.data.Forget(plugin)
	}
	if i.jobs != nil {
		i.jobs.ReplacePluginKinds(plugin, nil)
	}
	if i.spaces != nil {
		_ = i.spaces.Replace(plugin)
	}
	i.applySettings(plugin, nil)
	i.applyConnections(plugin, nil)
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
		if _, err := i.schedules.Create(ctx, jobs.ScheduleRequest{
			Name:     name,
			Kind:     kind.Name,
			Resource: access.PluginURI(manifest.ID),
			Cron:     declared.Cron,
		}); err != nil && !errors.Is(err, access.ErrConflict) {
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
	if _, err := i.Get(ctx, id); err != nil {
		return err
	}
	if err := i.checkInUse(ctx, id); err != nil {
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
	if purge {
		if err := i.store.Queries().DeletePluginDeclaration(ctx, id); err != nil {
			return err
		}
		if err := i.store.Queries().DeletePluginKV(ctx, id); err != nil {
			return fmt.Errorf("plugins: dropping private state: %w", err)
		}
		if err := i.store.Queries().DeletePluginEntities(ctx, "entity://"+id+"/"); err != nil {
			return fmt.Errorf("plugins: dropping entities: %w", err)
		}
		if i.sql != nil {
			if err := i.sql.DropSQLRole(ctx, id); err != nil {
				return err
			}
		}
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
	i.clearDeclarations(id)
	return nil
}

// SetEnabled turns a plugin on or off.
func (i *Installer) SetEnabled(ctx context.Context, id string, enabled bool) (Plugin, error) {
	i.mu.Lock()
	defer i.mu.Unlock()
	plugin, err := i.Get(ctx, id)
	if err != nil {
		return Plugin{}, err
	}
	if plugin.Enabled == enabled {
		return plugin, nil
	}
	manifest, err := pluginManifest(plugin)
	if err != nil {
		return Plugin{}, err
	}
	var prepared declarations
	if enabled {
		if err := i.checkRequirements(ctx, manifest); err != nil {
			return Plugin{}, err
		}
		prepared, err = i.prepare(manifest)
		if err != nil {
			return Plugin{}, err
		}
	}
	record, err := i.store.Queries().SetPluginEnabled(ctx, store.SetPluginEnabledParams{ID: id, Enabled: enabled})
	if err != nil {
		return Plugin{}, fmt.Errorf("plugins: changing %s: %w", id, err)
	}
	plugin, err = pluginOf(record)
	if err != nil {
		return Plugin{}, err
	}
	if err := i.activate(manifest, prepared, enabled); err != nil {
		return Plugin{}, err
	}
	if i.services != nil {
		if !enabled {
			i.services.Stop(id)
		} else if manifest.Service != nil && manifest.Service.Start != StartLazy {
			if err := i.services.Start(context.WithoutCancel(ctx), plugin); err != nil {
				i.logger.Error("starting plugin service", "plugin", id, "error", err.Error())
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

// Manifest returns the manifest of an enabled, installed plugin.
func (i *Installer) Manifest(ctx context.Context, id string) (*Manifest, error) {
	plugin, err := i.Get(ctx, id)
	if err != nil {
		return nil, err
	}
	if !plugin.Enabled {
		return nil, access.ErrNotFound
	}
	return pluginManifest(plugin)
}

func (i *Installer) applySettings(plugin string, manifest *Manifest) {
	if i.settings == nil {
		return
	}
	var definitions []config.Definition
	if manifest != nil {
		for name, setting := range manifest.Settings {
			value, err := cborx.Marshal(setting.Default)
			if err != nil {
				i.logger.Error("encoding a setting default", "plugin", plugin, "setting", name, "error", err.Error())
				continue
			}
			scopes := make([]string, 0, len(setting.Scopes))
			for _, scope := range setting.Scopes {
				scopes = append(scopes, string(scope))
			}
			definitions = append(definitions, config.Definition{Key: name, Type: string(setting.Type), Default: value, Scopes: scopes})
		}
	}
	i.settings.ReplacePluginDefinitions(plugin, definitions)
}

// InitSettings resolves the defaults and global values of a plugin.
func (i *Installer) InitSettings(ctx context.Context, plugin string) ([]byte, error) {
	manifest, err := i.Manifest(ctx, plugin)
	if err != nil {
		return nil, err
	}
	values := make(map[string]any, len(manifest.Settings))
	for name, setting := range manifest.Settings {
		values[name] = setting.Default
		if i.settings == nil {
			continue
		}
		value, err := i.settings.Get(ctx, plugin+"/"+name, access.Plugin(plugin), "")
		if err != nil {
			return nil, err
		}
		var decoded any
		if err := cborx.Unmarshal(value.Value, &decoded); err != nil {
			return nil, err
		}
		values[name] = decoded
	}
	return cborx.Marshal(values)
}

func (i *Installer) checkReverseDependencies(ctx context.Context, incoming *Manifest) error {
	installed, err := i.List(ctx)
	if err != nil {
		return err
	}
	for _, dependent := range installed {
		manifest, err := pluginManifest(dependent)
		if err != nil {
			return err
		}
		if requirement, ok := manifest.Dependencies[incoming.ID]; ok {
			if err := checkRequirement(incoming.Version, requirement); err != nil {
				return fmt.Errorf("plugin %s requires %s %s: %w", manifest.ID, incoming.ID, requirement, err)
			}
		}
	}
	return nil
}

func (i *Installer) checkInUse(ctx context.Context, plugin string) error {
	entry, err := i.store.Queries().PluginProvisionerUser(ctx, plugin)
	if err == nil {
		return fmt.Errorf("%w: provisioner %s uses plugin %s", access.ErrConflict, entry, plugin)
	}
	if !errors.Is(err, pgx.ErrNoRows) {
		return err
	}
	hosts, err := i.store.Queries().PluginHostProfiles(ctx)
	if err != nil {
		return err
	}
	for _, host := range hosts {
		var hostProfile profile.Profile
		if err := cborx.Unmarshal(host.Profile, &hostProfile); err != nil {
			return fmt.Errorf("plugins: reading profile of %s: %w", host.Name, err)
		}
		for _, feature := range hostProfile.Features {
			if feature.Plugin == plugin {
				return fmt.Errorf("%w: profile of %s uses plugin %s", access.ErrConflict, host.Name, plugin)
			}
		}
	}
	return nil
}

// Restore reconstructs installed files and in-memory declarations on startup.
func (i *Installer) Restore(ctx context.Context) error {
	i.mu.Lock()
	defer i.mu.Unlock()
	installed, err := i.List(ctx)
	if err != nil {
		return err
	}
	if i.data != nil {
		for _, collection := range i.data.Collections("") {
			if collection.Plugin != "rookery" {
				i.data.Forget(collection.Plugin)
			}
		}
	}
	for _, plugin := range installed {
		if err := i.restorePackage(ctx, plugin); err != nil {
			return fmt.Errorf("plugins: restoring %s: %w", plugin.ID, err)
		}
		if !plugin.Enabled {
			continue
		}
		manifest, err := pluginManifest(plugin)
		if err != nil {
			return err
		}
		prepared, err := i.prepare(manifest)
		if err != nil {
			return err
		}
		if err := i.activate(manifest, prepared, true); err != nil {
			return err
		}
		i.declareAtRuntime(ctx, manifest)
	}
	return nil
}

func (i *Installer) restorePackage(ctx context.Context, plugin Plugin) error {
	destination := i.Dir(plugin.ID, plugin.SHA256)
	if _, err := os.Stat(filepath.Join(destination, FileName)); err == nil {
		return nil
	}
	archive, err := i.store.Queries().GetPluginArchive(ctx, plugin.ID)
	if err != nil {
		return err
	}
	dir, err := os.MkdirTemp(i.stagingDir(), "restore-")
	if err != nil {
		return err
	}
	defer func() { _ = os.RemoveAll(dir) }()
	unpacked, err := Unpack(ctx, bytes.NewReader(archive), dir)
	if err != nil {
		return err
	}
	if unpacked.SHA256 != plugin.SHA256 || unpacked.Manifest.ID != plugin.ID {
		return fmt.Errorf("%w: stored package disagrees with its installed record", ErrPackage)
	}
	if err := os.RemoveAll(destination); err != nil {
		return err
	}
	_, err = movePackage(dir, destination)
	return err
}

// InstallBundled installs the first-party packages shipped beside the server.
func (i *Installer) InstallBundled(ctx context.Context, dir string) error {
	if dir == "" {
		return nil
	}
	for _, plugin := range []string{"docker", "incus"} {
		if _, err := i.Get(ctx, plugin); err == nil {
			continue
		} else if !errors.Is(err, access.ErrNotFound) {
			return err
		}
		if err := i.installBundle(ctx, dir, plugin); err != nil {
			return err
		}
	}
	return nil
}

func (i *Installer) installBundle(ctx context.Context, dir, plugin string) error {
	packageFile, err := os.Open(filepath.Join(dir, plugin+".tar.gz")) //nolint:gosec // configured release bundle directory
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("plugins: reading bundle %s: %w", plugin, err)
	}
	defer func() { _ = packageFile.Close() }()
	_, err = i.install(ctx, packageFile, installOptions{requireNew: true, expectedID: plugin})
	return err
}

func (i *Installer) applyConnections(plugin string, manifest *Manifest) {
	if i.connections == nil {
		return
	}
	var types []config.ConnectionType
	if manifest != nil {
		for _, declared := range manifest.ConnectionTypes {
			typ := config.ConnectionType{ID: declared.ID, Label: declared.Label, Test: declared.Test}
			for name, field := range declared.Fields {
				typ.Fields = append(typ.Fields, config.ConnectionField{Name: name, Secret: field.Secret})
			}
			types = append(types, typ)
		}
	}
	i.connections.ReplacePluginTypes(plugin, types)
}

// Run starts services whose declared job kinds have queued work.
func (i *Installer) Run(ctx context.Context) error {
	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return nil
		case <-ticker.C:
			queued, err := i.store.Queries().PluginsWithQueuedJobs(ctx)
			if err != nil {
				if ctx.Err() == nil {
					i.logger.Warn("looking for plugin work", "error", err.Error())
				}
				continue
			}
			for _, id := range queued {
				if i.services.Running(id) {
					continue
				}
				if plugin, err := i.Get(ctx, id); err == nil && plugin.Enabled {
					_ = i.services.Start(ctx, plugin)
				}
			}
		}
	}
}
