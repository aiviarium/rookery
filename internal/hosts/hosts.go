package hosts

import (
	"context"
	"errors"
	"fmt"
	"regexp"
	"sort"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/profile"
	"github.com/aiviarium/rookery/internal/store"
)

// Record is one host as the store keeps it: its name, what it can do now, the
// profile an apply makes of it, its labels, and what an apply last put on it.
type Record struct {
	Name string
	// Type is what the host can do now; Profile.Type is what it should be.
	Type    profile.Type
	Profile profile.Profile
	// Address is the host's tailnet address once it has joined one, empty before.
	Address string
	// AppliedDigest is the digest of the profile an apply last put on the host.
	AppliedDigest string
	AppliedAt     *time.Time
	LastSeenAt    *time.Time
	CreatedBy     string
	CreatedAt     time.Time
	UpdatedAt     time.Time
	Labels        map[string]string
}

// URI is the host's resource.
func (r Record) URI() string { return access.HostURI(r.Name) }

// Behind reports whether the host is running something other than what its
// profile asks for. A host that has never been applied is behind.
func (r Record) Behind(digest string) bool { return r.AppliedDigest == "" || r.AppliedDigest != digest }

// Hosts is the store of host records and labels.
type Hosts struct {
	store *store.Store
}

// New returns the host store for a database.
func New(opened *store.Store) *Hosts { return &Hosts{store: opened} }

// decode turns a row into a Record and reads its profile.
func (h *Hosts) decode(ctx context.Context, row store.RookeryHosts) (Record, error) {
	var p profile.Profile
	if err := cborx.Unmarshal(row.Profile, &p); err != nil {
		return Record{}, fmt.Errorf("hosts: reading the profile of %s: %w", row.Name, err)
	}
	hostType := profile.Type(row.Type)
	if !hostType.Valid() {
		return Record{}, fmt.Errorf("hosts: the row of %s has the type %q", row.Name, row.Type)
	}
	labels, err := h.labels(ctx, row.Name)
	if err != nil {
		return Record{}, err
	}
	record := Record{
		Name:          row.Name,
		Type:          hostType,
		Profile:       p,
		Address:       row.Address,
		AppliedDigest: row.AppliedDigest,
		AppliedAt:     row.AppliedAt,
		LastSeenAt:    row.LastSeenAt,
		CreatedAt:     row.CreatedAt,
		UpdatedAt:     row.UpdatedAt,
		Labels:        labels,
	}
	if row.CreatedBy != nil {
		record.CreatedBy = *row.CreatedBy
	}
	return record, nil
}

// labels is a host's labels.
func (h *Hosts) labels(ctx context.Context, name string) (map[string]string, error) {
	rows, err := h.store.Queries().ListHostLabels(ctx, name)
	if err != nil {
		return nil, fmt.Errorf("hosts: reading the labels of %s: %w", name, err)
	}
	labels := make(map[string]string, len(rows))
	for _, row := range rows {
		labels[row.Key] = row.Value
	}
	return labels, nil
}

// Get returns a host by name.
func (h *Hosts) Get(ctx context.Context, name string) (Record, error) {
	row, err := h.store.Queries().GetHost(ctx, name)
	if err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return Record{}, fmt.Errorf("%w: host %q", access.ErrNotFound, name)
		}
		return Record{}, fmt.Errorf("hosts: reading %s: %w", name, err)
	}
	return h.decode(ctx, row)
}

// Filter selects hosts for a list.
type Filter struct {
	// Labels selects hosts that have every one of these labels.
	Labels map[string]string
	// After is a name to start after, for a page.
	After string
	// Limit bounds the page; zero is the default of 100.
	Limit int
}

// List returns the hosts a filter selects, by name.
func (h *Hosts) List(ctx context.Context, filter Filter) ([]Record, error) {
	rows, err := h.store.Queries().ListHosts(ctx)
	if err != nil {
		return nil, fmt.Errorf("hosts: listing: %w", err)
	}
	var records []Record
	for _, row := range rows {
		if row.Name <= filter.After {
			continue
		}
		record, err := h.decode(ctx, row)
		if err != nil {
			return nil, err
		}
		if !hasLabels(record.Labels, filter.Labels) {
			continue
		}
		records = append(records, record)
		if filter.Limit > 0 && len(records) >= filter.Limit {
			break
		}
	}
	return records, nil
}

// Names is every host's name, for a roll-out or a follower to walk.
func (h *Hosts) Names(ctx context.Context) ([]string, error) {
	rows, err := h.store.Queries().ListHosts(ctx)
	if err != nil {
		return nil, fmt.Errorf("hosts: listing: %w", err)
	}
	names := make([]string, 0, len(rows))
	for _, row := range rows {
		names = append(names, row.Name)
	}
	sort.Strings(names)
	return names, nil
}

// hasLabels reports whether labels has every wanted pair.
func hasLabels(labels, wanted map[string]string) bool {
	for key, value := range wanted {
		if labels[key] != value {
			return false
		}
	}
	return true
}

// AddInput is a host to add.
type AddInput struct {
	Name    string
	Type    profile.Type
	Profile profile.Profile
	// Labels are the host's first labels.
	Labels map[string]string
	// CreatedBy is the user who added it, empty for the server itself.
	CreatedBy string
}

// Add records a host. The profile's type and the host's type are the same to
// begin with: an apply is what makes it grow.
func (h *Hosts) Add(ctx context.Context, input AddInput) (Record, error) {
	if err := validateName(input.Name); err != nil {
		return Record{}, err
	}
	p := input.Profile.Normalize()
	if input.Type.Valid() {
		p.Type = input.Type
	}
	if err := p.Validate(); err != nil {
		return Record{}, fmt.Errorf("%w: %w", access.ErrInvalid, err)
	}
	encoded, err := cborx.Marshal(p)
	if err != nil {
		return Record{}, fmt.Errorf("hosts: encoding the profile of %s: %w", input.Name, err)
	}
	var createdBy *string
	if input.CreatedBy != "" {
		createdBy = &input.CreatedBy
	}

	row, err := h.store.Queries().CreateHost(ctx, store.CreateHostParams{
		Name:      input.Name,
		Type:      string(p.Type),
		Profile:   encoded,
		CreatedBy: createdBy,
	})
	if err != nil {
		return Record{}, fmt.Errorf("hosts: adding %s: %w", input.Name, err)
	}
	if err := h.SetLabels(ctx, input.Name, input.Labels, nil); err != nil {
		return Record{}, err
	}
	return h.decode(ctx, row)
}

// SetProfile stores the desired configuration. Only a verified apply records
// what the host can do now.
func (h *Hosts) SetProfile(ctx context.Context, name string, p profile.Profile) (Record, error) {
	normalized := p.Normalize()
	if err := normalized.Validate(); err != nil {
		return Record{}, fmt.Errorf("%w: %w", access.ErrInvalid, err)
	}
	encoded, err := cborx.Marshal(normalized)
	if err != nil {
		return Record{}, fmt.Errorf("hosts: encoding the profile of %s: %w", name, err)
	}
	row, err := h.store.Queries().SetHostProfile(ctx, store.SetHostProfileParams{
		Name:    name,
		Profile: encoded,
	})
	if errors.Is(err, pgx.ErrNoRows) {
		return Record{}, fmt.Errorf("%w: host %q", access.ErrNotFound, name)
	}
	if err != nil {
		return Record{}, fmt.Errorf("hosts: setting the profile of %s: %w", name, err)
	}
	return h.decode(ctx, row)
}

// SetType stores the capabilities verified on a host.
func (h *Hosts) SetType(ctx context.Context, name string, hostType profile.Type) error {
	if !hostType.Valid() {
		return fmt.Errorf("%w: unknown host type", access.ErrInvalid)
	}
	return h.store.Queries().SetHostType(ctx, store.SetHostTypeParams{Name: name, Type: string(hostType)})
}

// SetLabels puts and takes labels.
func (h *Hosts) SetLabels(ctx context.Context, name string, set map[string]string, remove []string) error {
	keys := make([]string, 0, len(set))
	for key := range set {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	for _, key := range keys {
		if err := h.store.Queries().UpsertHostLabel(ctx, store.UpsertHostLabelParams{
			Host:  name,
			Key:   key,
			Value: set[key],
		}); err != nil {
			return fmt.Errorf("hosts: labelling %s: %w", name, err)
		}
	}
	for _, key := range remove {
		if err := h.store.Queries().DeleteHostLabel(ctx, store.DeleteHostLabelParams{Host: name, Key: key}); err != nil {
			return fmt.Errorf("hosts: unlabelling %s: %w", name, err)
		}
	}
	return nil
}

// Rename renames a host, with the grants and the resource its name was.
func (h *Hosts) Rename(ctx context.Context, name, newName string) (Record, error) {
	if err := validateName(newName); err != nil {
		return Record{}, err
	}
	row, err := h.store.Queries().RenameHost(ctx, store.RenameHostParams{Name: name, NewName: newName})
	if err != nil {
		return Record{}, fmt.Errorf("hosts: renaming %s: %w", name, err)
	}
	if err := h.moveResource(ctx, access.HostURI(name), access.HostURI(newName)); err != nil {
		return Record{}, err
	}
	return h.decode(ctx, row)
}

// Remove deletes a host with its labels, its grants and its resource.
func (h *Hosts) Remove(ctx context.Context, name string) error {
	deleted, err := h.store.Queries().DeleteHost(ctx, name)
	if err != nil {
		return fmt.Errorf("hosts: removing %s: %w", name, err)
	}
	if deleted == 0 {
		return fmt.Errorf("%w: host %q", access.ErrNotFound, name)
	}
	return h.dropResource(ctx, access.HostURI(name))
}

// moveResource moves a host's grants to its new resource.
func (h *Hosts) moveResource(ctx context.Context, from, to string) error {
	if err := h.store.Queries().UpsertResource(ctx, store.UpsertResourceParams{Uri: to}); err != nil {
		return fmt.Errorf("hosts: creating %s: %w", to, err)
	}
	grants, err := h.store.Queries().ListGrantsForResource(ctx, from)
	if err != nil {
		return fmt.Errorf("hosts: reading the grants of %s: %w", from, err)
	}
	for _, grant := range grants {
		if _, err := h.store.Queries().UpsertGrant(ctx, store.UpsertGrantParams{
			Resource:  to,
			Principal: grant.Principal,
			Level:     grant.Level,
		}); err != nil {
			return fmt.Errorf("hosts: moving a grant to %s: %w", to, err)
		}
	}
	return h.dropResource(ctx, from)
}

// dropResource deletes a resource and its grants.
func (h *Hosts) dropResource(ctx context.Context, uri string) error {
	if err := h.store.Queries().DeleteGrantsForResource(ctx, uri); err != nil {
		return fmt.Errorf("hosts: deleting the grants of %s: %w", uri, err)
	}
	if _, err := h.store.Queries().DeleteResource(ctx, uri); err != nil && !errors.Is(err, pgx.ErrNoRows) {
		return fmt.Errorf("hosts: deleting %s: %w", uri, err)
	}
	return nil
}

// MarkApplied stores what an apply did: the digest it applied, the type the host
// has now and the address it answered at.
func (h *Hosts) MarkApplied(ctx context.Context, name, digest string, hostType profile.Type, address string) (Record, error) {
	current, err := h.Get(ctx, name)
	if err != nil {
		return Record{}, err
	}
	if address == "" {
		address = current.Address
	}
	row, err := h.store.Queries().SetHostApplied(ctx, store.SetHostAppliedParams{
		Name:    name,
		Digest:  digest,
		Type:    string(Grow(current.Type, hostType)),
		Address: address,
	})
	if err != nil {
		return Record{}, fmt.Errorf("hosts: recording the apply of %s: %w", name, err)
	}
	return h.decode(ctx, row)
}

// Touch records that a link to the host answered.
func (h *Hosts) Touch(ctx context.Context, name string) error {
	if err := h.store.Queries().TouchHost(ctx, name); err != nil {
		return fmt.Errorf("hosts: touching %s: %w", name, err)
	}
	return nil
}

var hostName = regexp.MustCompile(`^[a-zA-Z0-9][a-zA-Z0-9._-]{0,63}$`)

func validateName(name string) error {
	if !hostName.MatchString(name) {
		return fmt.Errorf("%w: a host name must be one to 64 letters, digits, dots, underscores or dashes and start with a letter or digit", access.ErrInvalid)
	}
	return nil
}
