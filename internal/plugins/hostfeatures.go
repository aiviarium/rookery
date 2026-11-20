package plugins

import (
	"context"
	"fmt"
	"path/filepath"
	"runtime"
	"slices"
	"strings"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/apply"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/profile"
	"github.com/aiviarium/rookery/internal/provision"
)

type featureSecrets interface {
	Value(ctx context.Context, name, scope string) ([]byte, error)
}

// FeaturePlan is the installed plugin part of a host profile.
type FeaturePlan struct {
	Features []apply.Feature
	Actions  []apply.Action
	Offers   []string
	Services []string
	Topics   []string
}

// HostFeatures resolves a profile's declared features into their installed role,
// variables, services and provisioner offers. Secrets are optional for listing.
func (i *Installer) HostFeatures(ctx context.Context, desired profile.Profile, secrets featureSecrets) (FeaturePlan, error) {
	var resolved FeaturePlan
	for _, selected := range desired.Features {
		item, err := i.Get(ctx, selected.Plugin)
		if err != nil {
			return FeaturePlan{}, err
		}
		if !item.Enabled {
			return FeaturePlan{}, fmt.Errorf("%w: plugin %s is disabled", access.ErrUnavailable, item.ID)
		}
		manifest, err := pluginManifest(item)
		if err != nil {
			return FeaturePlan{}, err
		}
		at := slices.IndexFunc(manifest.HostFeatures, func(feature HostFeature) bool { return feature.ID == selected.Feature })
		if at == -1 {
			return FeaturePlan{}, fmt.Errorf("%w: plugin %s does not declare feature %s", access.ErrInvalid, item.ID, selected.Feature)
		}
		feature := manifest.HostFeatures[at]
		if len(feature.Types) != 0 && !slices.Contains(feature.Types, HostType(desired.Type)) {
			return FeaturePlan{}, fmt.Errorf("%w: %s/%s is unavailable for %s hosts", access.ErrInvalid, item.ID, feature.ID, desired.Type)
		}
		applyFeature, err := i.applyFeature(ctx, item, selected, feature, secrets)
		if err != nil {
			return FeaturePlan{}, err
		}
		resolved.Features = append(resolved.Features, applyFeature)
		for _, action := range feature.Actions {
			declared := apply.Action{Name: action.Name, Label: action.Label, User: string(action.User), OnHost: action.OnHost, Run: action.Run}
			for _, arg := range action.Args {
				declared.Args = append(declared.Args, apply.ActionArg{Name: arg.Name, Label: arg.Label, Required: arg.Required, Default: arg.Default})
			}
			resolved.Actions = append(resolved.Actions, declared)
		}
		resolved.Offers = append(resolved.Offers, feature.Offers...)
		resolved.Services = append(resolved.Services, feature.HostServices...)
		resolved.Topics = append(resolved.Topics, feature.ChannelTopics...)
	}
	return resolved, nil
}

func (i *Installer) applyFeature(ctx context.Context, plugin Plugin, selected profile.Feature, feature HostFeature, secrets featureSecrets) (apply.Feature, error) {
	values := make(map[string]any, len(feature.Vars))
	for name, variable := range feature.Vars {
		values[name] = variable.Default
	}
	if len(selected.Vars) != 0 {
		var provided map[string]any
		if err := cborx.Unmarshal(selected.Vars, &provided); err != nil {
			return apply.Feature{}, fmt.Errorf("%w: feature %s variables: %w", access.ErrInvalid, feature.ID, err)
		}
		for name, value := range provided {
			if _, ok := values[name]; !ok {
				return apply.Feature{}, fmt.Errorf("%w: feature %s does not declare variable %s", access.ErrInvalid, feature.ID, name)
			}
			values[name] = value
		}
	}
	encoded, err := cborx.Marshal(values)
	if err != nil {
		return apply.Feature{}, err
	}
	secretValues := make(map[string]string)
	if secrets != nil {
		for _, name := range feature.Secrets {
			value, err := secrets.Value(ctx, name, "plugin/"+plugin.ID)
			if err != nil {
				return apply.Feature{}, err
			}
			secretValues[name] = string(value)
		}
	}
	return apply.Feature{
		Plugin: plugin.ID, Feature: feature.ID, Dir: i.Dir(plugin.ID, plugin.SHA256),
		Path: feature.Tasks, Vars: encoded, Secrets: secretValues,
	}, nil
}

// ProvisionerTypes returns enabled provisioners with executables for this platform.
func (i *Installer) ProvisionerTypes(ctx context.Context) ([]provision.Type, error) {
	installed, err := i.List(ctx)
	if err != nil {
		return nil, err
	}
	var types []provision.Type
	for _, item := range installed {
		if !item.Enabled {
			continue
		}
		manifest, err := pluginManifest(item)
		if err != nil {
			return nil, err
		}
		for _, declared := range manifest.Provisioners {
			if executable := declared.Command[runtime.GOOS+"/"+runtime.GOARCH]; executable != "" {
				types = append(types, provision.Type{ID: item.ID + "/" + declared.ID, Plugin: item.ID,
					Label: declared.Label, Kinds: declared.Kinds, Command: filepath.Join(i.Dir(item.ID, item.SHA256), filepath.FromSlash(executable))})
			}
		}
	}
	slices.SortFunc(types, func(a, b provision.Type) int { return strings.Compare(a.ID, b.ID) })
	return types, nil
}

// ProvisionerType returns a named provisioner from the same catalog as listings.
func (i *Installer) ProvisionerType(ctx context.Context, id string) (provision.Type, bool, error) {
	types, err := i.ProvisionerTypes(ctx)
	if err != nil {
		return provision.Type{}, false, err
	}
	for _, declared := range types {
		if declared.ID == id {
			return declared, true, nil
		}
	}
	return provision.Type{}, false, nil
}
