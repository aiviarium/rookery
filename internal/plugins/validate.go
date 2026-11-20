package plugins

import (
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"regexp"
	"slices"
	"strconv"
	"strings"

	"github.com/robfig/cron/v3"
	"gopkg.in/yaml.v3"
)

// Load reads a plugin's manifest from the package in fsys, and checks it.
//
// It does not check that the files the manifest names are there; Check does that,
// and it is the check the installer and `rookery plugin check` use. Load is what
// a test with a manifest alone needs.
func Load(fsys fs.FS) (*Manifest, error) {
	file, err := fsys.Open(FileName)
	if err != nil {
		return nil, fmt.Errorf("%w: the package has no %s: %w", ErrInvalid, FileName, err)
	}
	defer func() { _ = file.Close() }()

	return parseManifest(file)
}

// loadFile reads a manifest from a file of the server's own, which is how an
// unpacked package's manifest is read.
func loadFile(name string) (*Manifest, error) {
	//nolint:gosec // the file is one the package was just unpacked to.
	file, err := os.Open(name)
	if err != nil {
		return nil, fmt.Errorf("plugins: reading %s: %w", name, err)
	}
	defer func() { _ = file.Close() }()
	return parseManifest(file)
}

// parseManifest decodes a manifest strictly, and checks it.
func parseManifest(r io.Reader) (*Manifest, error) {
	decoder := yaml.NewDecoder(r)
	// A manifest with a key Rookery does not know is a mistake, not something to
	// ignore: the key would otherwise silently do nothing.
	decoder.KnownFields(true)
	var manifest Manifest
	if err := decoder.Decode(&manifest); err != nil {
		if errors.Is(err, io.EOF) {
			return nil, fmt.Errorf("%w: %s is empty", ErrInvalid, FileName)
		}
		return nil, fmt.Errorf("%w: %s: %w", ErrInvalid, FileName, err)
	}
	if err := manifest.Validate(); err != nil {
		return nil, err
	}
	return &manifest, nil
}

// Check is Load, and then every file the manifest names: the service's binaries,
// its entry, each host feature's tasks and the client half.
func Check(fsys fs.FS) (*Manifest, error) {
	manifest, err := Load(fsys)
	if err != nil {
		return nil, err
	}
	var names []string
	if manifest.Service != nil {
		for _, path := range manifest.Service.Command {
			names = append(names, path)
		}
		names = append(names, manifest.Service.Entry)
	}
	for _, feature := range manifest.HostFeatures {
		names = append(names, feature.Tasks)
	}
	for _, provisioner := range manifest.Provisioners {
		for _, path := range provisioner.Command {
			names = append(names, path)
		}
	}
	for _, name := range names {
		if name == "" {
			continue
		}
		info, err := fs.Stat(fsys, strings.TrimSuffix(name, "/"))
		switch {
		case err != nil:
			return nil, fmt.Errorf("%w: the manifest names %s, which is not in the package: %w", ErrInvalid, name, err)
		case info.IsDir() && !strings.HasSuffix(name, "/"):
			return nil, fmt.Errorf("%w: the manifest names %s, which is a directory", ErrInvalid, name)
		}
	}
	if manifest.Client != "" {
		info, err := fs.Stat(fsys, strings.TrimSuffix(manifest.Client, "/"))
		if err != nil {
			return nil, fmt.Errorf("%w: the client half %s is not in the package: %w", ErrInvalid, manifest.Client, err)
		}
		if !info.IsDir() {
			return nil, fmt.Errorf("%w: the client half %s is not a directory", ErrInvalid, manifest.Client)
		}
	}
	return manifest, nil
}

var (
	pluginID  = regexp.MustCompile(`^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$`)
	plainName = regexp.MustCompile(`^[a-z][a-z0-9]*(?:[-_][a-z0-9]+)*$`)
	kindName  = regexp.MustCompile(`^[A-Z][A-Za-z0-9]*$`)
	procedure = regexp.MustCompile(`^[A-Za-z][A-Za-z0-9.]*/[A-Za-z][A-Za-z0-9]*$`)
	platform  = regexp.MustCompile(`^(linux|darwin|windows|freebsd)/(amd64|arm64|386|arm)$`)
	vector    = regexp.MustCompile(`^vector\(([0-9]+)\)$`)
	scopeName = regexp.MustCompile(`^[a-z][a-z0-9]*(?:[-/][a-z0-9]+)*$`)
	eventName = regexp.MustCompile(`^[a-z][a-z0-9]*(?:[-._][a-z0-9]+)*$`)
)

// Validate checks a manifest, and says what is wrong with it. It checks the
// manifest alone; Check also checks the files it names.
func (m *Manifest) Validate() error {
	if !pluginID.MatchString(m.ID) {
		return invalid("id %q is not a plugin id: lower-case letters, digits and hyphens", m.ID)
	}
	if m.Name == "" {
		return invalid("the plugin %s has no name", m.ID)
	}
	if _, err := releaseVersion(m.Version); err != nil {
		return err
	}
	if m.Requires != "" {
		if _, err := ParseRequirement(m.Requires); err != nil {
			return fmt.Errorf("plugin %s requires: %w", m.ID, err)
		}
	}
	if err := m.validateDependencies(); err != nil {
		return err
	}
	if err := m.validateService(); err != nil {
		return err
	}
	if err := m.validateCollections(); err != nil {
		return err
	}
	switch m.Database {
	case "", DatabaseSQL:
	default:
		return invalid("database %q: only sql is known", m.Database)
	}
	if err := m.validateSpaceKinds(); err != nil {
		return err
	}
	if err := m.validateSettings(); err != nil {
		return err
	}
	if err := m.validateNames("secret", m.Secrets); err != nil {
		return err
	}
	if err := m.validateConnectionTypes(); err != nil {
		return err
	}
	if err := m.validateJobs(); err != nil {
		return err
	}
	if err := m.validateSchedules(); err != nil {
		return err
	}
	if err := m.validateWebhooks(); err != nil {
		return err
	}
	if err := m.validateEvents(); err != nil {
		return err
	}
	if err := m.validateHostFeatures(); err != nil {
		return err
	}
	if err := m.validateProvisioners(); err != nil {
		return err
	}
	if err := m.validateClient(); err != nil {
		return err
	}
	return nil
}

// invalid is a refusal that names the manifest as the problem.
func invalid(format string, args ...any) error {
	return fmt.Errorf("%w: %s", ErrInvalid, fmt.Sprintf(format, args...))
}

// releaseVersion reads a manifest's version, which semver writes with all three
// numbers.
func releaseVersion(s string) (Version, error) {
	version, err := ParseVersion(s)
	if err != nil {
		return Version{}, err
	}
	parts := strings.Split(strings.Split(strings.TrimPrefix(s, "v"), "-")[0], ".")
	if len(parts) != 3 {
		return Version{}, invalid("version %q is not MAJOR.MINOR.PATCH", s)
	}
	return version, nil
}

func (m *Manifest) validateDependencies() error {
	for id, requirement := range m.Dependencies {
		if !pluginID.MatchString(id) {
			return invalid("dependency %q is not a plugin id", id)
		}
		if id == m.ID {
			return invalid("the plugin depends on itself")
		}
		if _, err := ParseRequirement(requirement); err != nil {
			return fmt.Errorf("dependency %s: %w", id, err)
		}
	}
	return nil
}

func (m *Manifest) validateService() error {
	s := m.Service
	if s == nil {
		return nil
	}
	hasBinaries := len(s.Command) > 0
	hasRuntime := s.Runtime != "" || s.Entry != ""
	switch {
	case hasBinaries && hasRuntime:
		return invalid("the service has both command and runtime: a plugin is one or the other")
	case !hasBinaries && !hasRuntime:
		return invalid("the service has neither command nor runtime and entry")
	}
	if hasBinaries {
		if _, ok := s.Command["linux/amd64"]; !ok {
			return invalid("the service has no linux/amd64 binary")
		}
		for name, path := range s.Command {
			if !platform.MatchString(name) {
				return invalid("service command %q is not GOOS/GOARCH", name)
			}
			if err := checkPath(path); err != nil {
				return fmt.Errorf("service command %s: %w", name, err)
			}
		}
	} else {
		if s.Runtime == "" || s.Entry == "" {
			return invalid("the service has a runtime or an entry but not both")
		}
		switch s.Runtime {
		case "bun", "node":
		default:
			return invalid("service runtime %q: bun or node", s.Runtime)
		}
		if err := checkPath(s.Entry); err != nil {
			return fmt.Errorf("service entry: %w", err)
		}
	}
	switch s.Start {
	case "", StartLazy, StartAlways:
	default:
		return invalid("service start %q: lazy or always", s.Start)
	}
	seen := make(map[string]bool, len(s.RPC))
	for _, rpc := range s.RPC {
		if !procedure.MatchString(rpc.Procedure) {
			return invalid("procedure %q is not Service/Method", rpc.Procedure)
		}
		if seen[rpc.Procedure] {
			return invalid("procedure %s is declared twice", rpc.Procedure)
		}
		seen[rpc.Procedure] = true
		if err := rpc.Access.validate(); err != nil {
			return fmt.Errorf("procedure %s: %w", rpc.Procedure, err)
		}
	}
	return nil
}

func (a Access) validate() error {
	if !slices.Contains([]Level{LevelView, LevelUse, LevelManage}, a.Level) {
		return invalid("access level %q is not view, use or manage", a.Level)
	}
	switch a.Resource {
	case ResourceHost, ResourceSpace, ResourcePlugin, ResourceRecord, ResourceDocument,
		ResourceProvisioner, ResourceRollout:
		return nil
	}
	return invalid("access resource %q is not one of host, space, plugin, record, document, provisioner and rollout", a.Resource)
}

func (m *Manifest) validateCollections() error {
	for name, collection := range m.Collections {
		if !plainName.MatchString(name) {
			return invalid("collection %q is not a collection name", name)
		}
		if err := collection.validate(name); err != nil {
			return err
		}
	}
	return nil
}

func (c Collection) validate(name string) error {
	switch c.Scope {
	case ScopeGlobal, ScopeSpace, ScopeUser:
	default:
		return invalid("collection %s: scope %q is not global, space or user", name, c.Scope)
	}
	if !slices.Contains([]Level{LevelView, LevelUse, LevelManage}, c.Access.Read) {
		return invalid("collection %s: read level %q is not view, use or manage", name, c.Access.Read)
	}
	if !slices.Contains([]Level{LevelView, LevelUse, LevelManage}, c.Access.Write) {
		return invalid("collection %s: write level %q is not view, use or manage", name, c.Access.Write)
	}
	if len(c.Fields) == 0 {
		return invalid("collection %s has no fields", name)
	}
	for field, declaration := range c.Fields {
		if !plainName.MatchString(field) {
			return invalid("collection %s: field %q is not a field name", name, field)
		}
		if err := declaration.validate(); err != nil {
			return fmt.Errorf("collection %s: field %s: %w", name, field, err)
		}
	}
	for _, index := range c.Indexes {
		if len(index) == 0 {
			return invalid("collection %s has an index on no field", name)
		}
		for _, field := range index {
			if _, ok := c.Fields[field]; !ok {
				return invalid("collection %s indexes %q, which is not a field", name, field)
			}
		}
	}
	if entity := c.Entity; entity != nil {
		if !kindName.MatchString(entity.Kind) {
			return invalid("collection %s: entity kind %q is not a kind", name, entity.Kind)
		}
		if _, ok := c.Fields[entity.Title]; !ok {
			return invalid("collection %s: the entity's title field %q is not a field", name, entity.Title)
		}
		for _, field := range entity.Text {
			declaration, ok := c.Fields[field]
			if !ok {
				return invalid("collection %s: the entity's text field %q is not a field", name, field)
			}
			if declaration.Type != FieldText && declaration.Type != FieldString {
				return invalid("collection %s: the entity's text field %q is not text", name, field)
			}
		}
	}
	return nil
}

func (f Field) validate() error {
	switch f.Type {
	case FieldString, FieldText, FieldInt, FieldFloat, FieldBool, FieldTime, FieldCBOR:
	case FieldEnum:
		if len(f.Enum) == 0 {
			return invalid("an enum field has no values")
		}
		for _, value := range f.Enum {
			if value == "" {
				return invalid("an enum field has an empty value")
			}
		}
	case FieldRef:
	default:
		match := vector.FindStringSubmatch(string(f.Type))
		if match == nil {
			return invalid("type %q is not a field type", f.Type)
		}
		dimension, err := strconv.Atoi(match[1])
		if err != nil || dimension < 1 || dimension > 20000 {
			return invalid("type %q: a vector holds 1 to 20000 values", f.Type)
		}
	}
	if len(f.Enum) > 0 && f.Type != FieldEnum {
		return invalid("only an enum field has values")
	}
	switch f.OnDelete {
	case "":
	case OnDeleteRestrict, OnDeleteNullify, OnDeleteCascade:
		if f.Type != FieldRef {
			return invalid("onDelete is for a ref field")
		}
	default:
		return invalid("onDelete %q is not restrict, nullify or cascade", f.OnDelete)
	}
	if f.Default != nil {
		if err := f.defaultMatches(); err != nil {
			return err
		}
	}
	return nil
}

// defaultMatches checks that a default is a value the field can hold.
func (f Field) defaultMatches() error {
	switch f.Type {
	case FieldEnum:
		value, ok := f.Default.(string)
		if !ok || !slices.Contains(f.Enum, value) {
			return invalid("default %v is not one of the enum's values", f.Default)
		}
	case FieldString, FieldText, FieldRef, FieldTime, FieldCBOR:
		if _, ok := f.Default.(string); !ok {
			return invalid("default %v is not a string", f.Default)
		}
	case FieldBool:
		if _, ok := f.Default.(bool); !ok {
			return invalid("default %v is not true or false", f.Default)
		}
	case FieldInt:
		if _, ok := f.Default.(int); !ok {
			return invalid("default %v is not a whole number", f.Default)
		}
	case FieldFloat:
		number, ok := f.Default.(float64)
		if !ok {
			if whole, ok := f.Default.(int); ok {
				number = float64(whole)
			} else {
				return invalid("default %v is not a number", f.Default)
			}
		}
		_ = number
	}
	return nil
}

func (m *Manifest) validateSpaceKinds() error {
	seen := make(map[string]bool, len(m.SpaceKinds))
	for _, kind := range m.SpaceKinds {
		if !plainName.MatchString(kind.ID) {
			return invalid("space kind %q is not a name", kind.ID)
		}
		if kind.Label == "" {
			return invalid("space kind %s has no label", kind.ID)
		}
		if seen[kind.ID] {
			return invalid("space kind %s is declared twice", kind.ID)
		}
		seen[kind.ID] = true
	}
	return nil
}

func (m *Manifest) validateSettings() error {
	for name, setting := range m.Settings {
		if !plainName.MatchString(name) {
			return invalid("setting %q is not a setting name", name)
		}
		if err := setting.validate(); err != nil {
			return fmt.Errorf("setting %s: %w", name, err)
		}
		seen := make(map[Scope]bool, len(setting.Scopes))
		for _, scope := range setting.Scopes {
			if !slices.Contains([]Scope{ScopeGlobal, ScopeSpace, ScopeUser}, scope) {
				return invalid("setting %s: scope %q is not global, space or user", name, scope)
			}
			if seen[scope] {
				return invalid("setting %s: scope %q is declared twice", name, scope)
			}
			seen[scope] = true
		}
	}
	return nil
}

func (v Variable) validate() error {
	switch v.Type {
	case VariableString, VariableInt, VariableFloat, VariableBool, VariableTime, VariableCBOR:
		return nil
	}
	return invalid("type %q is not string, int, float, bool, time or cbor", v.Type)
}

// validateNames checks a list of names, such as the secrets a plugin declares.
func (m *Manifest) validateNames(what string, names []string) error {
	seen := make(map[string]bool, len(names))
	for _, name := range names {
		if !plainName.MatchString(name) {
			return invalid("%s %q is not a name", what, name)
		}
		if seen[name] {
			return invalid("%s %s is declared twice", what, name)
		}
		seen[name] = true
	}
	return nil
}

func (m *Manifest) validateConnectionTypes() error {
	seen := make(map[string]bool, len(m.ConnectionTypes))
	for _, kind := range m.ConnectionTypes {
		if !plainName.MatchString(kind.ID) {
			return invalid("connection type %q is not a name", kind.ID)
		}
		if seen[kind.ID] {
			return invalid("connection type %s is declared twice", kind.ID)
		}
		seen[kind.ID] = true
		for name := range kind.Fields {
			if !plainName.MatchString(name) {
				return invalid("connection type %s: field %q is not a field name", kind.ID, name)
			}
		}
	}
	return nil
}

func (m *Manifest) validateJobs() error {
	seen := make(map[string]bool, len(m.Jobs))
	for _, job := range m.Jobs {
		if !plainName.MatchString(job.Kind) {
			return invalid("job kind %q is not a name", job.Kind)
		}
		if seen[job.Kind] {
			return invalid("job kind %s is declared twice", job.Kind)
		}
		seen[job.Kind] = true
		if err := job.Enqueue.validate(); err != nil {
			return fmt.Errorf("job %s: %w", job.Kind, err)
		}
		if job.Timeout < 0 {
			return invalid("job %s: timeout is negative", job.Kind)
		}
		if job.Retries < 0 {
			return invalid("job %s: retries is negative", job.Kind)
		}
	}
	return nil
}

func (m *Manifest) validateSchedules() error {
	for _, schedule := range m.Schedules {
		if err := m.hasJob(schedule.Kind); err != nil {
			return fmt.Errorf("schedule: %w", err)
		}
		if _, err := cron.ParseStandard(schedule.Cron); err != nil {
			return invalid("schedule for %s: %q is not a cron expression: %v", schedule.Kind, schedule.Cron, err)
		}
	}
	return nil
}

func (m *Manifest) validateWebhooks() error {
	seen := make(map[string]bool, len(m.Webhooks))
	for _, webhook := range m.Webhooks {
		if !plainName.MatchString(webhook.Name) {
			return invalid("webhook %q is not a name", webhook.Name)
		}
		if seen[webhook.Name] {
			return invalid("webhook %s is declared twice", webhook.Name)
		}
		seen[webhook.Name] = true
		if err := m.hasJob(webhook.Job); err != nil {
			return fmt.Errorf("webhook %s: %w", webhook.Name, err)
		}
		switch {
		case webhook.Verify.HMACSHA256 != nil && webhook.Verify.Token != nil:
			return invalid("webhook %s verifies two ways", webhook.Name)
		case webhook.Verify.HMACSHA256 != nil:
			hmac := webhook.Verify.HMACSHA256
			if hmac.Header == "" {
				return invalid("webhook %s: the HMAC has no header", webhook.Name)
			}
			if err := m.hasSecret(hmac.Secret); err != nil {
				return fmt.Errorf("webhook %s: %w", webhook.Name, err)
			}
		case webhook.Verify.Token != nil:
			if webhook.Verify.Token.Header == "" {
				return invalid("webhook %s: the token has no header", webhook.Name)
			}
			if err := m.hasSecret(webhook.Verify.Token.Secret); err != nil {
				return fmt.Errorf("webhook %s: %w", webhook.Name, err)
			}
		default:
			return invalid("webhook %s has no verification", webhook.Name)
		}
	}
	return nil
}

// hasJob checks that a job kind is one the plugin declares.
func (m *Manifest) hasJob(kind string) error {
	for _, job := range m.Jobs {
		if job.Kind == kind {
			return nil
		}
	}
	return invalid("job kind %q is not declared", kind)
}

// hasSecret checks that a secret is one the plugin declares.
func (m *Manifest) hasSecret(name string) error {
	if !slices.Contains(m.Secrets, name) {
		return invalid("secret %q is not declared", name)
	}
	return nil
}

func (m *Manifest) validateEvents() error {
	seen := make(map[string]bool, len(m.Events))
	for _, event := range m.Events {
		plugin, name, found := strings.Cut(event, "/")
		if !found || plugin != m.ID || !eventName.MatchString(name) {
			return invalid("event %q is not %s/name", event, m.ID)
		}
		if seen[event] {
			return invalid("event %s is declared twice", event)
		}
		seen[event] = true
	}
	return nil
}

func (m *Manifest) validateHostFeatures() error {
	seen := make(map[string]bool, len(m.HostFeatures))
	for _, feature := range m.HostFeatures {
		if !plainName.MatchString(feature.ID) {
			return invalid("host feature %q is not a name", feature.ID)
		}
		if seen[feature.ID] {
			return invalid("host feature %s is declared twice", feature.ID)
		}
		seen[feature.ID] = true
		if err := feature.validate(m); err != nil {
			return fmt.Errorf("host feature %s: %w", feature.ID, err)
		}
	}
	return nil
}

func (f HostFeature) validate(m *Manifest) error {
	if len(f.Types) == 0 {
		return invalid("it applies to no kind of host")
	}
	for _, hostType := range f.Types {
		if !slices.Contains([]HostType{HostSSH, HostManaged, HostWorkstation}, hostType) {
			return invalid("type %q is not ssh, managed or workstation", hostType)
		}
	}
	if err := checkPath(f.Tasks); err != nil {
		return fmt.Errorf("tasks: %w", err)
	}
	for name, variable := range f.Vars {
		if !plainName.MatchString(name) {
			return invalid("variable %q is not a name", name)
		}
		if err := variable.validate(); err != nil {
			return fmt.Errorf("variable %s: %w", name, err)
		}
	}
	for _, secret := range f.Secrets {
		if err := m.hasSecret(secret); err != nil {
			return err
		}
	}
	for _, offer := range f.Offers {
		if !scopeName.MatchString(offer) {
			return invalid("offer %q is not a provisioner type", offer)
		}
	}
	for _, service := range f.HostServices {
		if !scopeName.MatchString(service) {
			return invalid("host service %q is not plugin/name", service)
		}
	}
	for _, topic := range f.ChannelTopics {
		if !scopeName.MatchString(topic) {
			return invalid("channel topic %q is not plugin/topic", topic)
		}
	}
	actions := make(map[string]bool, len(f.Actions))
	for _, action := range f.Actions {
		if !plainName.MatchString(action.Name) {
			return invalid("action %q is not a name", action.Name)
		}
		if actions[action.Name] {
			return invalid("action %s is declared twice", action.Name)
		}
		actions[action.Name] = true
		if !slices.Contains([]ActionUser{ActionOperator, ActionRoot, ActionDesktop}, action.User) {
			return invalid("action %s: user %q is not operator, root or desktop", action.Name, action.User)
		}
		if strings.TrimSpace(action.Run) == "" {
			return invalid("action %s has nothing to run", action.Name)
		}
		args := make(map[string]bool, len(action.Args))
		for _, arg := range action.Args {
			if !plainName.MatchString(arg.Name) {
				return invalid("action %s: argument %q is not a name", action.Name, arg.Name)
			}
			if args[arg.Name] {
				return invalid("action %s: argument %s is declared twice", action.Name, arg.Name)
			}
			args[arg.Name] = true
		}
	}
	return nil
}

func (m *Manifest) validateProvisioners() error {
	seen := make(map[string]bool, len(m.Provisioners))
	for _, provisioner := range m.Provisioners {
		if !plainName.MatchString(provisioner.ID) {
			return invalid("provisioner %q is not a name", provisioner.ID)
		}
		if seen[provisioner.ID] {
			return invalid("provisioner %s is declared twice", provisioner.ID)
		}
		seen[provisioner.ID] = true
		if len(provisioner.Kinds) == 0 {
			return invalid("provisioner %s makes nothing", provisioner.ID)
		}
		for _, kind := range provisioner.Kinds {
			if !plainName.MatchString(kind) {
				return invalid("provisioner %s: kind %q is not a name", provisioner.ID, kind)
			}
		}
		if len(provisioner.Command) == 0 {
			return invalid("provisioner %s has no command", provisioner.ID)
		}
		if _, ok := provisioner.Command["linux/amd64"]; !ok {
			return invalid("provisioner %s has no linux/amd64 binary", provisioner.ID)
		}
		for name, path := range provisioner.Command {
			if !platform.MatchString(name) {
				return invalid("provisioner %s: command %q is not GOOS/GOARCH", provisioner.ID, name)
			}
			if err := checkPath(path); err != nil {
				return fmt.Errorf("provisioner %s: %w", provisioner.ID, err)
			}
		}
	}
	return nil
}

func (m *Manifest) validateClient() error {
	if m.Client == "" {
		if m.Shell {
			return invalid("a plugin without a client half cannot be the shell")
		}
		if m.Contributes != nil {
			return invalid("a plugin without a client half contributes nothing")
		}
		return nil
	}
	if err := checkPath(m.Client); err != nil {
		return fmt.Errorf("client: %w", err)
	}
	c := m.Contributes
	if c == nil {
		return nil
	}
	views := make(map[string]bool, len(c.Views))
	for _, view := range c.Views {
		if !plainName.MatchString(view.ID) {
			return invalid("view %q is not a name", view.ID)
		}
		if views[view.ID] {
			return invalid("view %s is declared twice", view.ID)
		}
		views[view.ID] = true
		if view.Label == "" {
			return invalid("view %s has no label", view.ID)
		}
		if !strings.HasPrefix(view.Route, "/") {
			return invalid("view %s: route %q does not start with /", view.ID, view.Route)
		}
	}
	for _, nav := range c.Nav {
		if !views[nav.View] {
			return invalid("navigation names view %q, which is not declared", nav.View)
		}
	}
	tabs := make(map[string]bool, len(c.HostTabs))
	for _, tab := range c.HostTabs {
		if !plainName.MatchString(tab.ID) {
			return invalid("host tab %q is not a name", tab.ID)
		}
		if tabs[tab.ID] {
			return invalid("host tab %s is declared twice", tab.ID)
		}
		tabs[tab.ID] = true
		for _, hostType := range tab.Types {
			if !slices.Contains([]HostType{HostSSH, HostManaged, HostWorkstation}, hostType) {
				return invalid("host tab %s: type %q is not ssh, managed or workstation", tab.ID, hostType)
			}
		}
	}
	for _, widget := range c.Widgets {
		if !plainName.MatchString(widget.ID) {
			return invalid("widget %q is not a name", widget.ID)
		}
	}
	for _, page := range c.AdminPages {
		if !plainName.MatchString(page.ID) {
			return invalid("admin page %q is not a name", page.ID)
		}
		if !strings.HasPrefix(page.Route, "/") {
			return invalid("admin page %s: route %q does not start with /", page.ID, page.Route)
		}
	}
	return nil
}

// checkPath checks a path in a package: relative, and inside it. A trailing
// slash is allowed, since a folder is often written with one.
func checkPath(path string) error {
	if path == "" {
		return invalid("it has no path")
	}
	if strings.HasPrefix(path, "/") {
		return invalid("path %q is not relative", path)
	}
	for _, part := range strings.Split(strings.TrimSuffix(path, "/"), "/") {
		if part == ".." || part == "." {
			return invalid("path %q leaves the package", path)
		}
	}
	return nil
}
