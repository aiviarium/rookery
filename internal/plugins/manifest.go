// Package plugins is a plugin's manifest, the package that carries it, and the
// registries packages come from.
//
// A .tar.gz with plugin.yaml at its root is a package. Load parses and validates
// the manifest, Check also checks the files it names, Pack writes a package and
// Unpack reads one, and Index is a registry's list of them, signed with Ed25519.
// The manifest's JSON Schema is generated from these Go types.
package plugins

import (
	"errors"
	"fmt"
	"time"

	"gopkg.in/yaml.v3"
)

// ErrInvalid is the sentinel every refusal of a manifest wraps.
var ErrInvalid = errors.New("plugins: invalid")

// FileName is the manifest's name at the root of a package.
const FileName = "plugin.yaml"

// Manifest is a plugin's plugin.yaml.
type Manifest struct {
	// ID is the plugin's id: lower case, and the name of its tables.
	ID string `yaml:"id" json:"id"`
	// Version is what this package is.
	Version string `yaml:"version" json:"version"`
	// Name is what people see.
	Name string `yaml:"name" json:"name"`
	// Requires is the range of Rookery versions this plugin works with, such as
	// ">=0.1.0".
	Requires string `yaml:"requires,omitempty" json:"requires,omitempty"`
	// Dependencies are the plugins and the versions of theirs this one needs.
	Dependencies map[string]string `yaml:"dependencies,omitempty" json:"dependencies,omitempty"`

	// Service is the plugin's server-side code, in any language with gRPC.
	Service *Service `yaml:"service,omitempty" json:"service,omitempty"`
	// Collections are the datastore collections the plugin declares, by name.
	Collections map[string]Collection `yaml:"collections,omitempty" json:"collections,omitempty"`
	// Database is "sql" for a plugin that wants a Postgres role of its own.
	Database Database `yaml:"database,omitempty" json:"database,omitempty"`

	// SpaceKinds are the kinds of space the plugin adds.
	SpaceKinds []SpaceKind `yaml:"spaceKinds,omitempty" json:"spaceKinds,omitempty"`
	// Settings are the settings the plugin declares, by name.
	Settings map[string]Variable `yaml:"settings,omitempty" json:"settings,omitempty"`
	// Secrets are the secrets the plugin declares, by name.
	Secrets []string `yaml:"secrets,omitempty" json:"secrets,omitempty"`
	// ConnectionTypes are the kinds of external connection the plugin adds.
	ConnectionTypes []ConnectionType `yaml:"connectionTypes,omitempty" json:"connectionTypes,omitempty"`
	// Jobs are the job kinds the plugin works.
	Jobs []Job `yaml:"jobs,omitempty" json:"jobs,omitempty"`
	// Schedules are the schedules the plugin starts with, when it is installed.
	Schedules []Schedule `yaml:"schedules,omitempty" json:"schedules,omitempty"`
	// Webhooks are the incoming webhooks the plugin takes.
	Webhooks []Webhook `yaml:"webhooks,omitempty" json:"webhooks,omitempty"`
	// Events are the events the plugin publishes, as "plugin/name".
	Events []string `yaml:"events,omitempty" json:"events,omitempty"`

	// HostFeatures are the features the plugin adds to hosts.
	HostFeatures []HostFeature `yaml:"hostFeatures,omitempty" json:"hostFeatures,omitempty"`
	// Provisioners are the provisioner types the plugin's package carries.
	Provisioners []Provisioner `yaml:"provisioners,omitempty" json:"provisioners,omitempty"`

	// Client is the folder of the plugin's client half, which the edge serves.
	Client string `yaml:"client,omitempty" json:"client,omitempty"`
	// Shell says this plugin's client half is the UI: Rookery serves its
	// index.html for every path no other route answers.
	Shell bool `yaml:"shell,omitempty" json:"shell,omitempty"`
	// Contributes is what the client half adds to the shell.
	Contributes *Contributions `yaml:"contributes,omitempty" json:"contributes,omitempty"`
}

// Service is a plugin's server-side code.
type Service struct {
	// Command is the executable to run, one per platform it was built for, as
	// "linux/amd64".
	Command map[string]string `yaml:"command,omitempty" json:"command,omitempty"`
	// Runtime is how to run Entry instead of a built binary: "bun" today.
	Runtime string `yaml:"runtime,omitempty" json:"runtime,omitempty"`
	// Entry is the file Runtime runs, when the plugin is not a binary.
	Entry string `yaml:"entry,omitempty" json:"entry,omitempty"`
	// Start is when the server starts the service: "lazy" on the first call, or
	// "always" with the server.
	Start StartMode `yaml:"start,omitempty" json:"start,omitempty"`
	// RPC are the service's procedures, and what a caller needs to reach them.
	RPC []RPC `yaml:"rpc,omitempty" json:"rpc,omitempty"`
}

// StartMode is when the server starts a plugin's service.
type StartMode string

// The start modes.
const (
	// StartLazy starts the service on its first call, after the server is up.
	StartLazy StartMode = "lazy"
	// StartAlways starts the service with the server.
	StartAlways StartMode = "always"
)

// RPC is one procedure a plugin's service serves.
type RPC struct {
	// Procedure is the fully qualified name, such as
	// example.v1.NoteService/Summarize.
	Procedure string `yaml:"procedure" json:"procedure"`
	// Access is what a caller needs, on the resource the request names.
	Access Access `yaml:"access" json:"access"`
}

// Access is a level on a kind of resource.
type Access struct {
	// Level is view, use or manage.
	Level Level `yaml:"level" json:"level"`
	// Resource is the request's resource: host, space, plugin, record, document,
	// provisioner or rollout.
	Resource Resource `yaml:"resource" json:"resource"`
}

// Level is what a caller may do with a resource.
type Level string

// The levels, from the least to the most.
const (
	LevelView   Level = "view"
	LevelUse    Level = "use"
	LevelManage Level = "manage"
)

// Resource is a kind of resource a level is given on.
type Resource string

// The resources an access option may name.
const (
	ResourceHost        Resource = "host"
	ResourceSpace       Resource = "space"
	ResourcePlugin      Resource = "plugin"
	ResourceRecord      Resource = "record"
	ResourceDocument    Resource = "document"
	ResourceProvisioner Resource = "provisioner"
	ResourceRollout     Resource = "rollout"
)

// Collection is one datastore collection a plugin declares.
type Collection struct {
	// Scope says which resource its records belong to: global, space or user.
	Scope Scope `yaml:"scope" json:"scope"`
	// Access is the level a caller needs to read and to write a record.
	Access CollectionAccess `yaml:"access" json:"access"`
	// History keeps every change of a record.
	History bool `yaml:"history,omitempty" json:"history,omitempty"`
	// Fields are the collection's, by name.
	Fields map[string]Field `yaml:"fields" json:"fields"`
	// Indexes are the fields to index, each list one index.
	Indexes [][]string `yaml:"indexes,omitempty" json:"indexes,omitempty"`
	// Entity makes a record of this collection something the entity registry
	// and search know.
	Entity *Entity `yaml:"entity,omitempty" json:"entity,omitempty"`
}

// Scope is what a collection's records belong to.
type Scope string

// The scopes.
const (
	ScopeGlobal Scope = "global"
	ScopeSpace  Scope = "space"
	ScopeUser   Scope = "user"
)

// CollectionAccess is the level a caller needs to read and to write.
type CollectionAccess struct {
	Read  Level `yaml:"read" json:"read"`
	Write Level `yaml:"write" json:"write"`
}

// Field is one field of a collection.
type Field struct {
	// Type is string, text, int, float, bool, time, enum, ref, vector(N) or
	// cbor.
	Type FieldType `yaml:"type" json:"type"`
	// Required refuses a record without the field.
	Required bool `yaml:"required,omitempty" json:"required,omitempty"`
	// Default is the value a record gets when it leaves the field out.
	Default any `yaml:"default,omitempty" json:"default,omitempty"`
	// Enum is the values an enum field has.
	Enum []string `yaml:"enum,omitempty" json:"enum,omitempty"`
	// OnDelete is what happens to this field when the record it refers to is
	// deleted: restrict, nullify or cascade.
	OnDelete OnDelete `yaml:"onDelete,omitempty" json:"onDelete,omitempty"`
}

// FieldType is a field's type.
type FieldType string

// The field types. A vector's dimension is part of the name, "vector(1536)".
const (
	FieldString FieldType = "string"
	FieldText   FieldType = "text"
	FieldInt    FieldType = "int"
	FieldFloat  FieldType = "float"
	FieldBool   FieldType = "bool"
	FieldTime   FieldType = "time"
	FieldEnum   FieldType = "enum"
	FieldRef    FieldType = "ref"
	FieldCBOR   FieldType = "cbor"
)

// OnDelete is what a reference does when its target goes.
type OnDelete string

// The ways a reference may end.
const (
	OnDeleteRestrict OnDelete = "restrict"
	OnDeleteNullify  OnDelete = "nullify"
	OnDeleteCascade  OnDelete = "cascade"
)

// Entity makes a collection's records entities.
type Entity struct {
	// Kind is what kind of thing a record is, such as Note.
	Kind string `yaml:"kind" json:"kind"`
	// Title is the field that names it.
	Title string `yaml:"title" json:"title"`
	// Text are the fields search reads.
	Text []string `yaml:"text,omitempty" json:"text,omitempty"`
}

// Database is a plugin's own database access.
type Database string

// DatabaseSQL asks for a Postgres role and a schema of the plugin's own.
const DatabaseSQL Database = "sql"

// SpaceKind is a kind of space a plugin adds.
type SpaceKind struct {
	ID    string `yaml:"id" json:"id"`
	Label string `yaml:"label" json:"label"`
}

// Variable is a setting the core or a plugin declares, or a feature's variable.
type Variable struct {
	// Type is string, int, float, bool, time or cbor.
	Type VariableType `yaml:"type" json:"type"`
	// Default is the value before anyone sets it.
	Default any `yaml:"default,omitempty" json:"default,omitempty"`
	// Scopes are where it may be set: global, space or user. Only settings have
	// them.
	Scopes []Scope `yaml:"scopes,omitempty" json:"scopes,omitempty"`
}

// VariableType is a setting's type.
type VariableType string

// The setting types.
const (
	VariableString VariableType = "string"
	VariableInt    VariableType = "int"
	VariableFloat  VariableType = "float"
	VariableBool   VariableType = "bool"
	VariableTime   VariableType = "time"
	VariableCBOR   VariableType = "cbor"
)

// ConnectionType is a kind of external connection a plugin adds.
type ConnectionType struct {
	ID     string                     `yaml:"id" json:"id"`
	Label  string                     `yaml:"label,omitempty" json:"label,omitempty"`
	Fields map[string]ConnectionField `yaml:"fields,omitempty" json:"fields,omitempty"`
	// Test says the plugin's service has a test for a connection of this type.
	Test bool `yaml:"test,omitempty" json:"test,omitempty"`
}

// ConnectionField is one field of a connection.
type ConnectionField struct {
	// Secret says the value is a secret, sealed and never shown again.
	Secret bool `yaml:"secret,omitempty" json:"secret,omitempty"`
}

// Job is a job kind a plugin's service works.
type Job struct {
	// Kind is the kind's name inside the plugin, such as reindex.
	Kind string `yaml:"kind" json:"kind"`
	// Enqueue is who may enqueue one.
	Enqueue Access `yaml:"enqueue" json:"enqueue"`
	// Timeout ends a job that runs too long.
	Timeout Duration `yaml:"timeout,omitempty" json:"timeout,omitempty"`
	// Retries is how often a failed job is tried again, with backoff.
	Retries int `yaml:"retries,omitempty" json:"retries,omitempty"`
}

// Schedule is a schedule the plugin starts with.
type Schedule struct {
	// Kind is the job kind it runs.
	Kind string `yaml:"kind" json:"kind"`
	// Cron is when it runs, in the server's time zone.
	Cron string `yaml:"cron" json:"cron"`
}

// Webhook is an incoming webhook a plugin takes.
type Webhook struct {
	Name string `yaml:"name" json:"name"`
	// Verify is how a request proves it is the plugin's.
	Verify WebhookVerify `yaml:"verify" json:"verify"`
	// Job is the job kind a verified request enqueues.
	Job string `yaml:"job" json:"job"`
}

// WebhookVerify is how an incoming webhook is verified: an HMAC over the body, or
// a token in a header. Exactly one of the two.
type WebhookVerify struct {
	// HMACSHA256 verifies an HMAC of the body.
	HMACSHA256 *HMAC `yaml:"hmac-sha256,omitempty" json:"hmac-sha256,omitempty"`
	// Token verifies a token in a header.
	Token *Token `yaml:"token,omitempty" json:"token,omitempty"`
}

// Token is a webhook's token: a header whose value must be a secret's.
type Token struct {
	// Header carries the token.
	Header string `yaml:"header" json:"header"`
	// Secret is the name of the secret the token must be.
	Secret string `yaml:"secret" json:"secret"`
}

// HMAC is an HMAC over a webhook's body.
type HMAC struct {
	// Header carries the signature.
	Header string `yaml:"header" json:"header"`
	// Secret is the name of the secret the signature is made with.
	Secret string `yaml:"secret" json:"secret"`
}

// HostFeature is a feature a plugin adds to hosts.
type HostFeature struct {
	ID    string `yaml:"id" json:"id"`
	Label string `yaml:"label,omitempty" json:"label,omitempty"`
	// Types are the kinds of host it applies to.
	Types []HostType `yaml:"types" json:"types"`
	// Tasks is the feature's Ansible tasks in the package, which an apply links
	// in as the role <plugin>__<feature>.
	Tasks string `yaml:"tasks" json:"tasks"`
	// Vars are the variables the feature's tasks read.
	Vars map[string]Variable `yaml:"vars,omitempty" json:"vars,omitempty"`
	// Secrets are the secrets the feature's tasks are given.
	Secrets []string `yaml:"secrets,omitempty" json:"secrets,omitempty"`
	// Offers are the provisioner types this feature's engine serves.
	Offers []string `yaml:"offers,omitempty" json:"offers,omitempty"`
	// HostServices are the host services it registers, for listings.
	HostServices []string `yaml:"hostServices,omitempty" json:"hostServices,omitempty"`
	// ChannelTopics are the topics it publishes on.
	ChannelTopics []string `yaml:"channelTopics,omitempty" json:"channelTopics,omitempty"`
	// Actions are the actions it adds to a host.
	Actions []Action `yaml:"actions,omitempty" json:"actions,omitempty"`
}

// HostType is a kind of host a feature applies to.
type HostType string

// The host types, as internal/hosts has them.
const (
	HostSSH         HostType = "ssh"
	HostManaged     HostType = "managed"
	HostWorkstation HostType = "workstation"
)

// Action is something an action does on a host, or a user does there.
type Action struct {
	Name  string `yaml:"name" json:"name"`
	Label string `yaml:"label,omitempty" json:"label,omitempty"`
	// Group is the section the action appears in.
	Group string `yaml:"group,omitempty" json:"group,omitempty"`
	// User is who it runs as: operator, root or desktop.
	User ActionUser `yaml:"user" json:"user"`
	// OnHost lets users on the host run it through box.
	OnHost bool `yaml:"onHost,omitempty" json:"onHost,omitempty"`
	// Terminal says it needs a terminal, which the server opens for it.
	Terminal bool `yaml:"terminal,omitempty" json:"terminal,omitempty"`
	// Run is the command, which runs with the arguments as ROOKERY_ARG_*
	// variables, never through a shell string.
	Run string `yaml:"run" json:"run"`
	// Args are the arguments the action takes.
	Args []ActionArg `yaml:"args,omitempty" json:"args,omitempty"`
}

// ActionUser is the account an action runs as.
type ActionUser string

// The accounts an action may run as.
const (
	ActionOperator ActionUser = "operator"
	ActionRoot     ActionUser = "root"
	ActionDesktop  ActionUser = "desktop"
)

// ActionArg is one argument of an action.
type ActionArg struct {
	Name     string `yaml:"name" json:"name"`
	Label    string `yaml:"label,omitempty" json:"label,omitempty"`
	Required bool   `yaml:"required,omitempty" json:"required,omitempty"`
	Default  string `yaml:"default,omitempty" json:"default,omitempty"`
}

// Provisioner is a provisioner type the plugin's package carries.
type Provisioner struct {
	ID    string `yaml:"id" json:"id"`
	Label string `yaml:"label,omitempty" json:"label,omitempty"`
	// Kinds are the resources it makes: machine, deployment, or one of its own.
	Kinds []string `yaml:"kinds" json:"kinds"`
	// Command is the executable to run, one per platform it was built for.
	Command map[string]string `yaml:"command" json:"command"`
}

// Contributions is what a plugin's client half adds to the shell.
type Contributions struct {
	Views      []View      `yaml:"views,omitempty" json:"views,omitempty"`
	Nav        []Nav       `yaml:"nav,omitempty" json:"nav,omitempty"`
	HostTabs   []HostTab   `yaml:"hostTabs,omitempty" json:"hostTabs,omitempty"`
	Widgets    []Widget    `yaml:"widgets,omitempty" json:"widgets,omitempty"`
	AdminPages []AdminPage `yaml:"adminPages,omitempty" json:"adminPages,omitempty"`
}

// View is a page a plugin's client half adds.
type View struct {
	ID    string `yaml:"id" json:"id"`
	Label string `yaml:"label" json:"label"`
	// Route is the path the shell shows it at.
	Route string `yaml:"route" json:"route"`
	// Space is the kind of space it needs, if any.
	Space string `yaml:"space,omitempty" json:"space,omitempty"`
}

// Nav is an entry in the shell's navigation.
type Nav struct {
	View string `yaml:"view" json:"view"`
	// Group is the section it sits in.
	Group string `yaml:"group,omitempty" json:"group,omitempty"`
}

// HostTab is a tab on a host's page.
type HostTab struct {
	ID    string     `yaml:"id" json:"id"`
	Label string     `yaml:"label" json:"label"`
	Types []HostType `yaml:"types,omitempty" json:"types,omitempty"`
}

// Widget is a tile a client half may show.
type Widget struct {
	ID    string `yaml:"id" json:"id"`
	Label string `yaml:"label,omitempty" json:"label,omitempty"`
	// View is the view it shows, when it shows one.
	View string `yaml:"view,omitempty" json:"view,omitempty"`
}

// AdminPage is an administrative page a client half adds.
type AdminPage struct {
	ID    string `yaml:"id" json:"id"`
	Label string `yaml:"label" json:"label"`
	Route string `yaml:"route" json:"route"`
}

// Duration is a length of time as a manifest writes it, such as "10m". It is a
// string in YAML, which time.Duration is not.
type Duration time.Duration

// UnmarshalYAML reads a duration such as "10m" or "1h30m".
func (d *Duration) UnmarshalYAML(node *yaml.Node) error {
	var text string
	if err := node.Decode(&text); err != nil {
		return fmt.Errorf("%w: a duration is a string such as 10m: %w", ErrInvalid, err)
	}
	parsed, err := time.ParseDuration(text)
	if err != nil {
		return fmt.Errorf("%w: %w", ErrInvalid, err)
	}
	*d = Duration(parsed)
	return nil
}

// MarshalYAML writes a duration as a string.
func (d Duration) MarshalYAML() (any, error) { return time.Duration(d).String(), nil }

// Duration is the duration as a time.Duration.
func (d Duration) Duration() time.Duration { return time.Duration(d) }
