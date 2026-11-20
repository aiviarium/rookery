package hosts

import (
	"context"
	"fmt"
	"os"
	"slices"
	"strings"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/apply"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/config"
	"github.com/aiviarium/rookery/internal/plugins"
	"github.com/aiviarium/rookery/internal/profile"
)

type featureSet struct {
	features []resolvedFeature
	versions map[string]string
	actions  []apply.Action
}

type resolvedFeature struct {
	apply.Feature
	secrets []string
}

func (o *Operations) resolve(ctx context.Context, p profile.Profile, hostType profile.Type) (featureSet, error) {
	resolved := featureSet{versions: make(map[string]string), actions: coreActions(hostType)}
	installed := make(map[string]*plugins.Manifest)
	folders := make(map[string]string)
	actionNames := make(map[string]bool)
	for _, action := range resolved.actions {
		actionNames[action.Name] = true
	}
	for _, feature := range p.Features {
		if o.options.Plugins == nil {
			return featureSet{}, fmt.Errorf("%w: no feature catalogue is configured", access.ErrUnavailable)
		}
		manifest := installed[feature.Plugin]
		if manifest == nil {
			pack, err := o.options.Plugins.Get(ctx, feature.Plugin)
			if err != nil {
				return featureSet{}, err
			}
			if !pack.Enabled {
				return featureSet{}, fmt.Errorf("%w: plugin %s is disabled", access.ErrUnavailable, feature.Plugin)
			}
			folder := o.options.Plugins.Dir(pack.ID, pack.SHA256)
			manifest, err = plugins.Load(os.DirFS(folder))
			if err != nil {
				return featureSet{}, fmt.Errorf("hosts: reading plugin %s: %w", pack.ID, err)
			}
			installed[feature.Plugin], folders[feature.Plugin] = manifest, folder
			resolved.versions[feature.Plugin] = pack.Version
		}
		if err := resolved.add(feature, manifest, folders[feature.Plugin], hostType, actionNames); err != nil {
			return featureSet{}, err
		}
	}
	return resolved, nil
}

func (f *featureSet) add(feature profile.Feature, manifest *plugins.Manifest, folder string, hostType profile.Type, names map[string]bool) error {
	index := slices.IndexFunc(manifest.HostFeatures, func(declaration plugins.HostFeature) bool {
		return declaration.ID == feature.Feature
	})
	if index < 0 {
		return fmt.Errorf("%w: no feature %s/%s", access.ErrNotFound, feature.Plugin, feature.Feature)
	}
	spec := manifest.HostFeatures[index]
	if !slices.Contains(spec.Types, plugins.HostType(hostType)) {
		return fmt.Errorf("%w: feature %s/%s does not support %s hosts", access.ErrUnavailable, feature.Plugin, feature.Feature, hostType)
	}
	vars, err := featureVariables(feature, spec)
	if err != nil {
		return err
	}
	f.features = append(f.features, resolvedFeature{Feature: apply.Feature{
		Plugin: feature.Plugin, Feature: feature.Feature, Dir: folder, Path: spec.Tasks, Vars: vars,
	}, secrets: spec.Secrets})
	for _, declaration := range spec.Actions {
		if names[declaration.Name] {
			return fmt.Errorf("%w: action %q is defined more than once on the host", access.ErrConflict, declaration.Name)
		}
		names[declaration.Name] = true
		action := apply.Action{
			Name: declaration.Name, Label: declaration.Label, Group: declaration.Group, User: string(declaration.User),
			OnHost: declaration.OnHost, Terminal: declaration.Terminal, Run: declaration.Run,
		}
		if action.Label == "" {
			action.Label = action.Name
		}
		if action.Group == "" {
			action.Group = feature.Plugin
		}
		for _, arg := range declaration.Args {
			action.Args = append(action.Args, apply.ActionArg{Name: arg.Name, Label: arg.Label, Required: arg.Required, Default: arg.Default})
		}
		f.actions = append(f.actions, action)
	}
	return nil
}

func featureVariables(feature profile.Feature, spec plugins.HostFeature) ([]byte, error) {
	fromProfile := make(map[string]any)
	if len(feature.Vars) > 0 {
		if err := cborx.Unmarshal(feature.Vars, &fromProfile); err != nil {
			return nil, fmt.Errorf("%w: variables for %s: %w", access.ErrInvalid, feature.Role(), err)
		}
	}
	values := make(map[string]any, len(spec.Vars))
	for name, value := range spec.Vars {
		if value.Default != nil {
			values[name] = value.Default
		}
	}
	for name, value := range fromProfile {
		if _, found := spec.Vars[name]; !found {
			return nil, fmt.Errorf("%w: feature %s does not declare variable %q", access.ErrInvalid, feature.Role(), name)
		}
		values[name] = value
	}
	return cborx.Marshal(values)
}

func (o *Operations) applyFeatures(ctx context.Context, resolved featureSet) ([]apply.Feature, error) {
	features := make([]apply.Feature, 0, len(resolved.features))
	for _, source := range resolved.features {
		feature := source.Feature
		feature.Secrets = make(map[string]string, len(source.secrets))
		for _, name := range source.secrets {
			if o.options.Secrets == nil {
				return nil, fmt.Errorf("%w: no secret store is configured", access.ErrUnavailable)
			}
			value, err := o.options.Secrets.Value(ctx, name, config.SecretScopePlugin+feature.Plugin)
			if err != nil {
				return nil, fmt.Errorf("hosts: loading secret %s for %s: %w", name, feature.Role(), err)
			}
			feature.Secrets[name] = string(value)
		}
		features = append(features, feature)
	}
	return features, nil
}

func (o *Operations) registries(ctx context.Context, names []string) ([]apply.Registry, error) {
	if len(names) > 0 && o.options.Connections == nil {
		return nil, fmt.Errorf("%w: no connection store is configured", access.ErrUnavailable)
	}
	result := make([]apply.Registry, 0, len(names))
	for _, name := range names {
		values, err := o.options.Connections.NamedValues(ctx, strings.TrimSpace(name), "registry")
		if err != nil {
			return nil, err
		}
		result = append(result, apply.Registry{Address: values["url"], Username: values["username"], Password: values["password"]})
	}
	return result, nil
}
