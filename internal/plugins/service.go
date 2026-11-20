package plugins

import (
	"context"
	"fmt"
	"io"
	"os"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
)

// PluginService is PluginService: List, Get, Install, Remove and SetEnabled.
type PluginService struct {
	installer  *Installer
	registries *Registries
}

// NewService returns PluginService for an installer.
func NewService(installer *Installer) *PluginService {
	return &PluginService{installer: installer, registries: NewRegistries(installer.store)}
}

// List returns the installed plugins.
func (s *PluginService) List(ctx context.Context, _ *connect.Request[rookeryv1.PluginServiceListRequest]) (*connect.Response[rookeryv1.PluginServiceListResponse], error) {
	found, err := s.installer.List(ctx)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := make([]*rookeryv1.Plugin, 0, len(found))
	for _, plugin := range found {
		out = append(out, plugin.Message())
	}
	return connect.NewResponse(&rookeryv1.PluginServiceListResponse{Plugins: out}), nil
}

// Get returns one plugin.
func (s *PluginService) Get(ctx context.Context, request *connect.Request[rookeryv1.PluginServiceGetRequest]) (*connect.Response[rookeryv1.PluginServiceGetResponse], error) {
	plugin, err := s.installer.Get(ctx, request.Msg.GetId())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.PluginServiceGetResponse{Plugin: plugin.Message()}), nil
}

// Install streams a package in, then installs it.
func (s *PluginService) Install(ctx context.Context, stream *connect.ClientStream[rookeryv1.PluginServiceInstallRequest]) (*connect.Response[rookeryv1.PluginServiceInstallResponse], error) {
	packageFile, err := os.CreateTemp(s.installer.stagingDir(), "package-")
	if err != nil {
		return nil, access.ToConnect(fmt.Errorf("plugins: making a place for the package: %w", err))
	}
	defer func() {
		_ = packageFile.Close()
		_ = os.Remove(packageFile.Name())
	}()

	var size int64
	for stream.Receive() {
		chunk := stream.Msg().GetChunk()
		size += int64(len(chunk))
		if size > MaxPackageSize {
			return nil, access.ToConnect(fmt.Errorf("%w: the package is larger than %d bytes", ErrPackage, MaxPackageSize))
		}
		if _, err := packageFile.Write(chunk); err != nil {
			return nil, access.ToConnect(fmt.Errorf("plugins: writing the package: %w", err))
		}
	}
	if err := stream.Err(); err != nil {
		return nil, err
	}
	if size == 0 {
		return nil, access.ToConnect(fmt.Errorf("%w: the package is empty", ErrPackage))
	}
	if _, err := packageFile.Seek(0, io.SeekStart); err != nil {
		return nil, access.ToConnect(fmt.Errorf("plugins: reading the package back: %w", err))
	}

	plugin, err := s.installer.install(ctx, packageFile, installOptions{requireNew: true})
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.PluginServiceInstallResponse{Plugin: plugin.Message()}), nil
}

// Remove takes a plugin away.
func (s *PluginService) Remove(ctx context.Context, request *connect.Request[rookeryv1.PluginServiceRemoveRequest]) (*connect.Response[rookeryv1.PluginServiceRemoveResponse], error) {
	if err := s.installer.Remove(ctx, request.Msg.GetId(), request.Msg.GetPurge()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.PluginServiceRemoveResponse{}), nil
}

// SetEnabled turns a plugin on or off.
func (s *PluginService) SetEnabled(ctx context.Context, request *connect.Request[rookeryv1.PluginServiceSetEnabledRequest]) (*connect.Response[rookeryv1.PluginServiceSetEnabledResponse], error) {
	plugin, err := s.installer.SetEnabled(ctx, request.Msg.GetId(), request.Msg.GetEnabled())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.PluginServiceSetEnabledResponse{Plugin: plugin.Message()}), nil
}

// InstallFromRegistry verifies an indexed package before installing it.
func (s *PluginService) InstallFromRegistry(ctx context.Context, request *connect.Request[rookeryv1.PluginServiceInstallFromRegistryRequest]) (*connect.Response[rookeryv1.PluginServiceInstallFromRegistryResponse], error) {
	plugin, err := s.registries.install(ctx, s.installer, request.Msg.GetReference(), installOptions{requireNew: true})
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.PluginServiceInstallFromRegistryResponse{Plugin: plugin.Message()}), nil
}

// Update installs a signed release of an already installed plugin.
func (s *PluginService) Update(ctx context.Context, request *connect.Request[rookeryv1.PluginServiceUpdateRequest]) (*connect.Response[rookeryv1.PluginServiceUpdateResponse], error) {
	plugin, err := s.registries.install(ctx, s.installer, request.Msg.GetReference(), installOptions{requireExisting: true, resetData: request.Msg.GetResetData()})
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.PluginServiceUpdateResponse{Plugin: plugin.Message()}), nil
}

// Contributions lists the enabled client halves and their immutable hashes.
func (s *PluginService) Contributions(ctx context.Context, _ *connect.Request[rookeryv1.PluginServiceContributionsRequest]) (*connect.Response[rookeryv1.PluginServiceContributionsResponse], error) {
	declared, err := s.installer.Contributions(ctx)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := &rookeryv1.PluginServiceContributionsResponse{}
	for _, contribution := range declared {
		out.Contributions = append(out.Contributions, &rookeryv1.PluginContribution{
			Plugin: contribution.Plugin, Hash: contribution.Hash, Shell: contribution.Shell, ContributesJson: contribution.Contributes,
		})
	}
	return connect.NewResponse(out), nil
}
