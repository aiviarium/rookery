package docs

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/data"
	"github.com/aiviarium/rookery/internal/store"
)

// Snapshot is a compacted state of a document.
type Snapshot struct {
	// DocumentID is the document it belongs to.
	DocumentID string
	// Sequence is the last update it covers.
	Sequence int64
	// Version is true when it is a version: the document's history.
	Version bool
	// Label is what a version is called.
	Label string
	// CreatedBy is who wrote it, as access writes a principal.
	CreatedBy string
	// At is when it was written.
	At time.Time
}

// Snapshot stores a compacted state of a document, covering the sequence it
// names. A version is a snapshot kept in the document's history and needs
// `manage` on it; a compaction needs `use`.
func (d *Documents) Snapshot(ctx context.Context, principal data.Principal, id string, sequence int64, data []byte, version bool, label string) (Snapshot, error) {
	if len(data) == 0 {
		return Snapshot{}, fmt.Errorf("%w: a snapshot has bytes", ErrInvalid)
	}
	if sequence < 0 {
		return Snapshot{}, fmt.Errorf("%w: a sequence is not negative", ErrInvalid)
	}
	need := writeLevel
	if version {
		need = manageLevel
	}
	if _, err := d.requireDocument(ctx, principal, id, need); err != nil {
		return Snapshot{}, err
	}

	row, err := d.store.Queries().PutDocumentSnapshot(ctx, store.PutDocumentSnapshotParams{
		DocumentID: id, Seq: sequence, Data: data, Version: version, Label: &label, CreatedBy: principal.String(),
	})
	if errors.Is(err, pgx.ErrNoRows) {
		return Snapshot{}, fmt.Errorf("%w: snapshot sequence %d is beyond the document head or already a version", ErrInvalid, sequence)
	}
	if err != nil {
		return Snapshot{}, fmt.Errorf("docs: writing a snapshot: %w", err)
	}
	return Snapshot{
		DocumentID: row.DocumentID, Sequence: row.Seq, Version: row.Version,
		Label: label, CreatedBy: row.CreatedBy, At: row.CreatedAt,
	}, nil
}

// History lists a document's versions, oldest first. The caller needs `view`.
func (d *Documents) History(ctx context.Context, principal data.Principal, id string) ([]Snapshot, error) {
	if _, err := d.requireDocument(ctx, principal, id, readLevel); err != nil {
		return nil, err
	}

	rows, err := d.store.Queries().ListDocumentVersions(ctx, id)
	if err != nil {
		return nil, fmt.Errorf("docs: reading the document's history: %w", err)
	}
	snapshots := make([]Snapshot, 0, len(rows))
	for _, row := range rows {
		var label string
		if row.Label != nil {
			label = *row.Label
		}
		snapshots = append(snapshots, Snapshot{
			DocumentID: row.DocumentID, Sequence: row.Seq, Version: row.Version,
			Label: label, CreatedBy: row.CreatedBy, At: row.CreatedAt.UTC(),
		})
	}
	return snapshots, nil
}

// transaction runs a function in one transaction.
func (d *Documents) transaction(ctx context.Context, run func(tx pgx.Tx) error) error {
	tx, err := d.store.Pool().Begin(ctx)
	if err != nil {
		return fmt.Errorf("docs: beginning the transaction: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	if err := run(tx); err != nil {
		return err
	}
	if err := tx.Commit(ctx); err != nil {
		return fmt.Errorf("docs: committing the transaction: %w", err)
	}
	return nil
}
