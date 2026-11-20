package messaging

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/store"
)

// Leases are named locks with a holder, a TTL and a fencing number that rises on
// each acquire, so a holder that lost its lease can be told from the one that
// has it now.
type Leases struct {
	store *store.Store
}

// NewLeases returns the leases of a database.
func NewLeases(opened *store.Store) *Leases { return &Leases{store: opened} }

// Lease is a held lock.
type Lease struct {
	Name    string
	Holder  string
	Fencing int64
	Expires time.Time
}

// LeaseOf is a lease from the database.
func LeaseOf(record store.RookeryLeases) Lease {
	return Lease{Name: record.Name, Holder: record.Holder, Fencing: record.Fencing, Expires: record.ExpiresAt}
}

// ErrLeaseHeld is a lease another holder has.
var ErrLeaseHeld = fmt.Errorf("%w: the lease is held by somebody else", access.ErrConflict)

// Acquire takes a lease, or renews one this holder already has. The fencing
// number rises on every acquire, and is what the holder shows to prove it still
// holds the lease.
func (l *Leases) Acquire(ctx context.Context, name, holder string, ttl time.Duration) (Lease, error) {
	if name == "" || holder == "" {
		return Lease{}, fmt.Errorf("%w: a lease needs a name and a holder", access.ErrInvalid)
	}
	if ttl <= 0 {
		ttl = time.Minute
	}

	record, err := l.store.Queries().AcquireLease(ctx, store.AcquireLeaseParams{
		Name:   name,
		Holder: holder,
		Ttl:    interval(ttl),
	})
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return Lease{}, ErrLeaseHeld
	case err != nil:
		return Lease{}, fmt.Errorf("messaging: taking the lease %s: %w", name, err)
	}
	return LeaseOf(record), nil
}

// Renew extends a lease the holder still has.
func (l *Leases) Renew(ctx context.Context, name, holder string, fencing int64, ttl time.Duration) error {
	renewed, err := l.store.Queries().RenewLease(ctx, store.RenewLeaseParams{
		Name:    name,
		Holder:  holder,
		Fencing: fencing,
		Ttl:     interval(ttl),
	})
	switch {
	case err != nil:
		return fmt.Errorf("messaging: renewing the lease %s: %w", name, err)
	case renewed == 0:
		return ErrLeaseHeld
	}
	return nil
}

// Release lets a lease go.
func (l *Leases) Release(ctx context.Context, name, holder string, fencing int64) error {
	released, err := l.store.Queries().ReleaseLease(ctx, store.ReleaseLeaseParams{
		Name:    name,
		Holder:  holder,
		Fencing: fencing,
	})
	switch {
	case err != nil:
		return fmt.Errorf("messaging: releasing the lease %s: %w", name, err)
	case released == 0:
		return ErrLeaseHeld
	}
	return nil
}

// Held returns a lease, or access.ErrNotFound when it has expired.
func (l *Leases) Held(ctx context.Context, name string) (Lease, error) {
	record, err := l.store.Queries().GetLease(ctx, name)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return Lease{}, fmt.Errorf("%w: no lease %s", access.ErrNotFound, name)
	case err != nil:
		return Lease{}, fmt.Errorf("messaging: reading the lease %s: %w", name, err)
	case !record.ExpiresAt.After(time.Now()):
		return Lease{}, fmt.Errorf("%w: the lease %s has expired", access.ErrNotFound, name)
	}
	return LeaseOf(record), nil
}
