package plugins

import (
	"context"
	"crypto/ed25519"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/store"
)

const maxIndexSize = 8 << 20

// Registries stores the trusted keys and resolves signed package references.
type Registries struct {
	store  *store.Store
	client *http.Client
}

// NewRegistries opens the deployment's trusted package sources.
func NewRegistries(opened *store.Store) *Registries {
	return &Registries{store: opened, client: &http.Client{
		Timeout: time.Minute,
		CheckRedirect: func(req *http.Request, _ []*http.Request) error {
			return fmt.Errorf("registry: redirects are not supported (status from %s)", req.URL.Host)
		},
	}}
}

func registryURL(raw string) (*url.URL, error) {
	u, err := url.Parse(raw)
	if err != nil || u.Scheme != "https" || u.Hostname() == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" {
		return nil, fmt.Errorf("%w: registry URLs must be HTTPS URLs without credentials, queries or fragments", access.ErrInvalid)
	}
	return u, nil
}

// Add saves a trusted source.
func (r *Registries) Add(ctx context.Context, request *connect.Request[rookeryv1.RegistryServiceAddRequest]) (*connect.Response[rookeryv1.RegistryServiceAddResponse], error) {
	registry := request.Msg.GetRegistry()
	if !pluginID.MatchString(registry.GetName()) || len(registry.GetPublicKey()) != ed25519.PublicKeySize {
		return nil, access.ToConnect(fmt.Errorf("%w: a registry needs a name and an Ed25519 public key", access.ErrInvalid))
	}
	u, err := registryURL(registry.GetUrl())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	count, err := r.store.Queries().AddRegistry(ctx, store.AddRegistryParams{
		Name: registry.GetName(), Url: strings.TrimRight(u.String(), "/") + "/", PublicKey: registry.GetPublicKey(),
	})
	if err != nil {
		return nil, access.ToConnect(fmt.Errorf("registry: adding %s: %w", registry.GetName(), err))
	}
	if count == 0 {
		return nil, access.ToConnect(fmt.Errorf("%w: the registry %s already exists", access.ErrConflict, registry.GetName()))
	}
	return connect.NewResponse(&rookeryv1.RegistryServiceAddResponse{}), nil
}

// List returns the trusted sources and their public keys.
func (r *Registries) List(ctx context.Context, _ *connect.Request[rookeryv1.RegistryServiceListRequest]) (*connect.Response[rookeryv1.RegistryServiceListResponse], error) {
	rows, err := r.store.Queries().ListRegistries(ctx)
	if err != nil {
		return nil, access.ToConnect(fmt.Errorf("registry: listing registries: %w", err))
	}
	result := &rookeryv1.RegistryServiceListResponse{}
	for _, row := range rows {
		result.Registries = append(result.Registries, &rookeryv1.Registry{Name: row.Name, Url: row.Url, PublicKey: row.PublicKey})
	}
	return connect.NewResponse(result), nil
}

// Remove deletes a trusted source.
func (r *Registries) Remove(ctx context.Context, request *connect.Request[rookeryv1.RegistryServiceRemoveRequest]) (*connect.Response[rookeryv1.RegistryServiceRemoveResponse], error) {
	count, err := r.store.Queries().DeleteRegistry(ctx, request.Msg.GetName())
	if err != nil {
		return nil, access.ToConnect(fmt.Errorf("registry: deleting registry: %w", err))
	}
	if count == 0 {
		return nil, access.ToConnect(fmt.Errorf("%w: registry %s", access.ErrNotFound, request.Msg.GetName()))
	}
	return connect.NewResponse(&rookeryv1.RegistryServiceRemoveResponse{}), nil
}

func (r *Registries) install(ctx context.Context, installer *Installer, reference string) (Plugin, error) {
	id, version, pinned := strings.Cut(reference, "@")
	if !pluginID.MatchString(id) {
		return Plugin{}, fmt.Errorf("%w: expected ID or ID@VERSION", access.ErrInvalid)
	}
	if pinned {
		if _, err := ParseVersion(version); err != nil {
			return Plugin{}, fmt.Errorf("%w: %w", access.ErrInvalid, err)
		}
	}
	rows, err := r.store.Queries().ListRegistries(ctx)
	if err != nil {
		return Plugin{}, fmt.Errorf("registry: listing registries: %w", err)
	}
	for _, row := range rows {
		plugin, err := r.installFrom(ctx, installer, row, id, reference)
		if !errors.Is(err, ErrNotFound) {
			if err != nil {
				return Plugin{}, fmt.Errorf("registry %s: %w", row.Name, err)
			}
			return plugin, nil
		}
	}
	return Plugin{}, fmt.Errorf("%w: no registry has %s", access.ErrNotFound, reference)
}

func (r *Registries) installFrom(ctx context.Context, installer *Installer, row store.RookeryRegistries, id, reference string) (Plugin, error) {
	base, err := registryURL(row.Url)
	if err != nil {
		return Plugin{}, err
	}
	index, err := r.fetchIndex(ctx, base, ed25519.PublicKey(row.PublicKey))
	if err != nil {
		return Plugin{}, err
	}
	entry, err := index.Resolve(reference)
	if err != nil {
		return Plugin{}, err
	}
	target, err := base.Parse(entry.URL)
	if err != nil {
		return Plugin{}, fmt.Errorf("%w: invalid package URL in registry index", access.ErrInvalid)
	}
	if _, err := registryURL(target.String()); err != nil {
		return Plugin{}, err
	}
	if entry.Size < 1 || entry.Size > MaxPackageSize {
		return Plugin{}, fmt.Errorf("%w: invalid package size in registry index", access.ErrInvalid)
	}
	file, err := os.CreateTemp(installer.stagingDir(), "registry-")
	if err != nil {
		return Plugin{}, fmt.Errorf("registry: staging package: %w", err)
	}
	defer func() {
		_ = file.Close()
		_ = os.Remove(file.Name())
	}()
	hash := sha256.New()
	n, err := r.fetch(ctx, target, io.MultiWriter(file, hash), entry.Size)
	if err != nil {
		return Plugin{}, err
	}
	want, err := hex.DecodeString(entry.SHA256)
	if err != nil || n != entry.Size || subtle.ConstantTimeCompare(want, hash.Sum(nil)) != 1 {
		return Plugin{}, fmt.Errorf("%w: the package does not match the signed size and hash", access.ErrInvalid)
	}
	manifest, err := ManifestOf(io.NewSectionReader(file, 0, n))
	if err != nil {
		return Plugin{}, fmt.Errorf("%w: %w", access.ErrInvalid, err)
	}
	if manifest.ID != id || manifest.Version != entry.Version || manifest.Requires != entry.Requires {
		return Plugin{}, fmt.Errorf("%w: the package manifest does not match the signed index", access.ErrInvalid)
	}
	return installer.install(ctx, io.NewSectionReader(file, 0, n), true)
}

func (r *Registries) fetchIndex(ctx context.Context, base *url.URL, key ed25519.PublicKey) (Index, error) {
	var index, signature strings.Builder
	if _, err := r.fetch(ctx, base.JoinPath("index.json"), &index, maxIndexSize); err != nil {
		return Index{}, err
	}
	if _, err := r.fetch(ctx, base.JoinPath("index.json.sig"), &signature, ed25519.SignatureSize); err != nil {
		return Index{}, err
	}
	result, err := LoadIndex([]byte(index.String()), []byte(signature.String()), key)
	if err != nil {
		return Index{}, fmt.Errorf("%w: %w", access.ErrInvalid, err)
	}
	return result, nil
}

func (r *Registries) fetch(ctx context.Context, u *url.URL, into io.Writer, limit int64) (int64, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u.String(), nil)
	if err != nil {
		return 0, fmt.Errorf("registry: building request: %w", err)
	}
	resp, err := r.client.Do(req)
	if err != nil {
		return 0, fmt.Errorf("%w: fetching %s: %w", access.ErrUnavailable, u.Host, err)
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode != http.StatusOK {
		return 0, fmt.Errorf("%w: %s answered with HTTP %d", access.ErrUnavailable, u.Host, resp.StatusCode)
	}
	n, err := io.Copy(into, io.LimitReader(resp.Body, limit+1))
	if err != nil {
		return 0, fmt.Errorf("registry: reading response: %w", err)
	}
	if n > limit {
		return 0, fmt.Errorf("%w: registry response exceeds %d bytes", access.ErrInvalid, limit)
	}
	return n, nil
}
