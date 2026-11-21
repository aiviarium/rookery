// Package access is who may do what to which resource: levels, resource URIs,
// grants, the interceptor that enforces them, and the one place errors become
// connect codes.
//
// A resource is a URI: host://NAME, space://ID, record://PLUGIN/COLLECTION/ID,
// document://ID, plugin://ID, provisioner://NAME, rollout://ID, job://ID. A
// resource can have a parent, kept in rookery.resources, and a grant on a parent
// applies to its children.
//
// Levels are view ⊂ use ⊂ manage, held on a resource by a user, a group or
// everyone. Admins hold manage everywhere and the Unix socket's local admin
// holds everything. On top of the chain, a procedure may need a level that is
// not granted: admin (the caller is an admin), self (the call is about the
// caller) or local (the caller is the Unix socket).
package access

import (
	"context"
	"fmt"
	"strings"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
)

// Level is the access a principal holds or a procedure needs. It is the proto
// enum so that a service and an interceptor compare the same values.
type Level = rookeryv1.Level

// The levels of the chain, shortest first.
const (
	View   = rookeryv1.Level_LEVEL_VIEW
	Use    = rookeryv1.Level_LEVEL_USE
	Manage = rookeryv1.Level_LEVEL_MANAGE
)

// The levels a procedure can require that are not granted on a resource.
const (
	Admin = rookeryv1.Level_LEVEL_ADMIN
	Self  = rookeryv1.Level_LEVEL_SELF
	Local = rookeryv1.Level_LEVEL_LOCAL
)

// Satisfies reports whether holding held is enough for need, inside the
// view ⊂ use ⊂ manage chain.
func Satisfies(held, need Level) bool {
	return held != rookeryv1.Level_LEVEL_UNSPECIFIED && held >= need
}

// ParseLevel reads a level by name, as a flag or a manifest has it.
func ParseLevel(text string) (Level, error) {
	switch strings.ToLower(strings.TrimSpace(text)) {
	case "view":
		return View, nil
	case "use":
		return Use, nil
	case "manage":
		return Manage, nil
	case "admin":
		return Admin, nil
	case "self":
		return Self, nil
	case "local":
		return Local, nil
	default:
		return rookeryv1.Level_LEVEL_UNSPECIFIED, fmt.Errorf("access: %q is not a level: view, use, manage, admin, self or local", text)
	}
}

// LevelName is a level in words, for a log line or a message.
func LevelName(level Level) string {
	switch level {
	case View:
		return "view"
	case Use:
		return "use"
	case Manage:
		return "manage"
	case Admin:
		return "admin"
	case Self:
		return "self"
	case Local:
		return "local"
	default:
		return "none"
	}
}

// Kinds of resource a URI can name.
const (
	KindHost        = "host"
	KindSpace       = "space"
	KindRecord      = "record"
	KindDocument    = "document"
	KindPlugin      = "plugin"
	KindProvisioner = "provisioner"
	KindRollout     = "rollout"
	KindJob         = "job"
)

// kindFields are the request field names that name a kind of resource. The
// access interceptor turns the field's value into a URI of that kind.
var kindFields = map[string]bool{
	KindHost:        true,
	KindSpace:       true,
	KindDocument:    true,
	KindRecord:      true,
	KindPlugin:      true,
	KindProvisioner: true,
	KindRollout:     true,
	KindJob:         true,
}

// HostURI is a host's resource.
func HostURI(name string) string { return KindHost + "://" + name }

// SpaceURI is a space's resource.
func SpaceURI(id string) string { return KindSpace + "://" + id }

// RecordURI is a record's resource.
func RecordURI(plugin, collection, id string) string {
	return KindRecord + "://" + plugin + "/" + collection + "/" + id
}

// DocumentURI is a document's resource.
func DocumentURI(id string) string { return KindDocument + "://" + id }

// PluginURI is a plugin's resource.
func PluginURI(id string) string { return KindPlugin + "://" + id }

// ProvisionerURI is a provisioner entry's resource.
func ProvisionerURI(name string) string { return KindProvisioner + "://" + name }

// RolloutURI is a roll-out's resource.
func RolloutURI(id string) string { return KindRollout + "://" + id }

// JobURI is a job's resource.
func JobURI(id string) string { return KindJob + "://" + id }

// ParseResource checks a resource URI and returns its kind; the rest of the URI
// is the kind's to read.
func ParseResource(uri string) (string, error) {
	kind, rest, found := strings.Cut(uri, "://")
	if !found || kind == "" || rest == "" {
		return "", fmt.Errorf("%w: %q is not a resource URI, such as space://01h…", ErrInvalid, uri)
	}
	switch kind {
	case KindHost, KindSpace, KindDocument, KindPlugin, KindProvisioner, KindRollout, KindJob, KindUser, "blob":
		// A provisioner entry's name holds the type it names, so it holds a
		// slash: "provisioner://lab-docker/container" is the entry an apply
		// made for the docker/container type on lab.
		if strings.ContainsAny(rest, "/?#") && kind != KindRecord && kind != KindProvisioner {
			return "", fmt.Errorf("%w: %q has more than an id after %s://", ErrInvalid, uri, kind)
		}
	case KindRecord:
		if parts := strings.Split(rest, "/"); len(parts) != 3 || parts[0] == "" || parts[1] == "" || parts[2] == "" {
			return "", fmt.Errorf("%w: %q is not a record URI, such as record://plugin/collection/01h…", ErrInvalid, uri)
		}
	default:
		return "", fmt.Errorf("%w: %q names the unknown kind %q", ErrInvalid, uri, kind)
	}
	return kind, nil
}

// Principal kinds.
const (
	KindUser            = "user"
	KindPluginPrincipal = "plugin"
	KindLocal           = "local"
)

// Principal is who is calling.
type Principal struct {
	// Kind is "user", "plugin" or "local".
	Kind string
	// ID is the user's or the plugin's id. The local admin has none.
	ID string
	// Email and Name say more about a user, for an audit line and a greeting.
	Email string
	Name  string
	// Admin is set for a user with the admin flag.
	Admin bool
}

// LocalAdmin is the Unix socket's principal: every level everywhere.
func LocalAdmin() Principal { return Principal{Kind: KindLocal, ID: KindLocal} }

// Plugin is a plugin service's principal.
func Plugin(id string) Principal { return Principal{Kind: KindPluginPrincipal, ID: id} }

// UserID is the user's id, or empty when a plugin or the Unix socket calls. It
// is what a column that references rookery.users records as who did a thing.
func (p Principal) UserID() string {
	if p.Kind != KindUser {
		return ""
	}
	return p.ID
}

// IsLocal reports whether the principal is the Unix socket's admin.
func (p Principal) IsLocal() bool { return p.Kind == KindLocal }

// Authenticated reports whether anybody is calling.
func (p Principal) Authenticated() bool { return p.Kind != "" }

// IsAdmin reports whether the principal holds every level everywhere.
func (p Principal) IsAdmin() bool { return p.Admin || p.IsLocal() }

// String is how the principal is written in a grant and an audit line:
// "user:01h…", "plugin:example" or "local".
func (p Principal) String() string {
	if !p.Authenticated() {
		return ""
	}
	if p.IsLocal() {
		return KindLocal
	}
	return p.Kind + ":" + p.ID
}

// principals are the grant principals a caller matches: its own, "everyone",
// and its groups. The groups are looked up with the level in one query.
func (p Principal) grantPrincipals(groups []string) []string {
	out := make([]string, 0, len(groups)+2)
	if p.Kind == KindUser || p.Kind == KindPluginPrincipal {
		out = append(out, p.String())
	}
	out = append(out, Everyone)
	for _, group := range groups {
		out = append(out, GroupPrincipal(group))
	}
	return out
}

// Everyone is the principal that means every signed-in caller.
const Everyone = "everyone"

// UserPrincipal is a grant's user principal.
func UserPrincipal(id string) string { return KindUser + ":" + id }

// GroupPrincipal is a grant's group principal.
func GroupPrincipal(id string) string { return "group:" + id }

type principalKey struct{}

// WithPrincipal puts the caller in the context. The edge's authentication
// middleware is the only caller of this in the server.
func WithPrincipal(ctx context.Context, principal Principal) context.Context {
	return context.WithValue(ctx, principalKey{}, principal)
}

// PrincipalFrom reads the caller from the context.
func PrincipalFrom(ctx context.Context) (Principal, bool) {
	principal, ok := ctx.Value(principalKey{}).(Principal)
	return principal, ok && principal.Authenticated()
}

// MustPrincipal is PrincipalFrom for code that has already refused an
// unauthenticated caller.
func MustPrincipal(ctx context.Context) Principal {
	principal, _ := PrincipalFrom(ctx)
	return principal
}

type resourceKey struct{}

// WithResource puts the resource a call was about in the context, so that the
// audit interceptor records it without reading the request again.
func WithResource(ctx context.Context, resource string) context.Context {
	return context.WithValue(ctx, resourceKey{}, resource)
}

// ResourceFrom reads the resource the interceptor resolved for the call, or the
// empty string when the procedure names none.
func ResourceFrom(ctx context.Context) string {
	resource, _ := ctx.Value(resourceKey{}).(string)
	return resource
}

type spaceKey struct{}

// WithSpace puts the caller's current space, the Rookery-Space header, in the
// context.
func WithSpace(ctx context.Context, space string) context.Context {
	return context.WithValue(ctx, spaceKey{}, space)
}

// SpaceFrom reads the caller's current space.
func SpaceFrom(ctx context.Context) string {
	space, _ := ctx.Value(spaceKey{}).(string)
	return space
}
