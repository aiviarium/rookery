package provision

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/store"
)

// Resource is something a provisioner entry made: a machine, a deployment, or a
// kind of the provisioner's own.
type Resource struct {
	// ID is the core's id for it.
	ID string

	// Kind is machine, deployment, or the provisioner's own.
	Kind string

	// Name is what the entry called it, and for a machine the host's name.
	Name string

	// Provisioner is the entry's name.
	Provisioner string

	// State is what the provisioner stored to reach it again.
	State []byte

	// Address is where it answers, when it has one.
	Address string

	CreatedAt time.Time
	UpdatedAt time.Time
}

// Resources records what provisioner entries made.
type Resources struct {
	store *store.Store
}

// NewResources returns the resources of a database.
func NewResources(opened *store.Store) *Resources { return &Resources{store: opened} }

// Create records a resource, refusing a second machine for one host.
func (r *Resources) Create(ctx context.Context, resource Resource) (Resource, error) {
	if resource.Kind == "" {
		return Resource{}, fmt.Errorf("%w: a resource needs a kind", access.ErrInvalid)
	}
	if resource.Name == "" {
		return Resource{}, fmt.Errorf("%w: a resource needs a name", access.ErrInvalid)
	}
	row, err := r.store.Queries().CreateProvisionedResource(ctx, store.CreateProvisionedResourceParams{
		ID:          resource.ID,
		Kind:        resource.Kind,
		Name:        resource.Name,
		Provisioner: resource.Provisioner,
		State:       resource.State,
		Address:     resource.Address,
	})
	if err != nil {
		if isUniqueViolation(err) {
			if resource.Kind == KindMachine {
				return Resource{}, fmt.Errorf("%w: %s is already a machine of a host", access.ErrConflict, resource.Name)
			}
			return Resource{}, fmt.Errorf("%w: %s made a %s called %s already", access.ErrConflict, resource.Provisioner, resource.Kind, resource.Name)
		}
		return Resource{}, fmt.Errorf("provision: recording the %s %s: %w", resource.Kind, resource.Name, err)
	}
	return resourceOf(row), nil
}

// ByID returns a resource, or access.ErrNotFound.
func (r *Resources) ByID(ctx context.Context, id string) (Resource, error) {
	row, err := r.store.Queries().GetProvisionedResource(ctx, id)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return Resource{}, fmt.Errorf("%w: no resource %s", access.ErrNotFound, id)
	case err != nil:
		return Resource{}, fmt.Errorf("provision: reading the resource %s: %w", id, err)
	}
	return resourceOf(row), nil
}

// MachineOf is the machine a host has, if it has one.
func (r *Resources) MachineOf(ctx context.Context, host string) (Resource, bool, error) {
	row, err := r.store.Queries().GetProvisionedMachine(ctx, host)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return Resource{}, false, nil
	case err != nil:
		return Resource{}, false, fmt.Errorf("provision: reading the machine of %s: %w", host, err)
	}
	return resourceOf(row), true, nil
}

// List is a page of what an entry made, by id.
func (r *Resources) List(ctx context.Context, provisioner, kind, cursor string, limit int) ([]Resource, error) {
	params := store.ListProvisionedResourcesParams{
		Cursor:   cursor,
		RowLimit: clampLimit(limit),
	}
	if provisioner != "" {
		params.Provisioner = &provisioner
	}
	if kind != "" {
		params.Kind = &kind
	}
	rows, err := r.store.Queries().ListProvisionedResources(ctx, params)
	if err != nil {
		return nil, fmt.Errorf("provision: listing resources: %w", err)
	}
	out := make([]Resource, 0, len(rows))
	for _, row := range rows {
		out = append(out, resourceOf(row))
	}
	return out, nil
}

// Update records a resource's new state and address.
func (r *Resources) Update(ctx context.Context, id string, state []byte, address string) (Resource, error) {
	row, err := r.store.Queries().UpdateProvisionedResource(ctx, store.UpdateProvisionedResourceParams{
		ID:      id,
		State:   state,
		Address: address,
	})
	if errors.Is(err, pgx.ErrNoRows) {
		return Resource{}, fmt.Errorf("%w: no resource %s", access.ErrNotFound, id)
	}
	if err != nil {
		return Resource{}, fmt.Errorf("provision: updating the resource %s: %w", id, err)
	}
	return resourceOf(row), nil
}

// Delete forgets a resource, after the provisioner removed it.
func (r *Resources) Delete(ctx context.Context, id string) error {
	if _, err := r.store.Queries().DeleteProvisionedResource(ctx, id); err != nil {
		return fmt.Errorf("provision: deleting the resource %s: %w", id, err)
	}
	return nil
}

func resourceOf(row store.RookeryProvisionedResources) Resource {
	return Resource{
		ID:          row.ID,
		Kind:        row.Kind,
		Name:        row.Name,
		Provisioner: row.Provisioner,
		State:       row.State,
		Address:     row.Address,
		CreatedAt:   row.CreatedAt,
		UpdatedAt:   row.UpdatedAt,
	}
}
