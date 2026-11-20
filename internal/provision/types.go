package provision

import (
	"context"
	"sort"
)

// Kind is a resource kind: the core's machine, or one a provisioner declares.
type Kind struct {
	// ID is the kind's name: machine, deployment, or the provisioner's own.
	ID    string
	Label string

	// Machine says this is the core's kind: a machine for a host, at most one
	// per host.
	Machine bool
}

// CoreKinds are the kinds the core knows.
func CoreKinds() []Kind {
	return []Kind{{ID: KindMachine, Label: "Machine", Machine: true}}
}

// IsMachineKind reports whether a kind is the core's machine.
func IsMachineKind(kind string) bool { return kind == KindMachine }

// Type is a provisioner type an installed plugin declares, with the executable
// that serves it.
type Type struct {
	// ID is the type's name, "plugin/id", such as "docker/container".
	ID string

	// Plugin is the plugin whose package carries it.
	Plugin string

	Label string

	// Kinds are the resource kinds it makes.
	Kinds []string

	// Command is the executable's absolute path in the installed plugin's
	// folder.
	Command string
}

// KindList is the type's kinds, as values.
func (t Type) KindList() []Kind {
	kinds := make([]Kind, 0, len(t.Kinds))
	for _, id := range t.Kinds {
		if IsMachineKind(id) {
			kinds = append(kinds, Kind{ID: id, Label: "Machine", Machine: true})
			continue
		}
		kinds = append(kinds, Kind{ID: id, Label: id})
	}
	return kinds
}

// Catalog is where the core asks what provisioner types the installed plugins
// declare. The plugin system implements it from the installed manifests.
type Catalog interface {
	// ProvisionerTypes is every type the installed plugins declare, by id.
	ProvisionerTypes(ctx context.Context) ([]Type, error)

	// ProvisionerType is one type by its "plugin/id" name. It returns false
	// when no installed plugin declares it.
	ProvisionerType(ctx context.Context, id string) (Type, bool, error)
}

// TypesOf is a catalog built from a fixed list of types, for a test or a host
// feature's offers.
type TypesOf []Type

// ProvisionerTypes is every type of the list, by id.
func (t TypesOf) ProvisionerTypes(context.Context) ([]Type, error) {
	out := make([]Type, len(t))
	copy(out, t)
	sort.Slice(out, func(i, j int) bool { return out[i].ID < out[j].ID })
	return out, nil
}

// ProvisionerType is one type of the list.
func (t TypesOf) ProvisionerType(_ context.Context, id string) (Type, bool, error) {
	for _, candidate := range t {
		if candidate.ID == id {
			return candidate, true, nil
		}
	}
	return Type{}, false, nil
}
