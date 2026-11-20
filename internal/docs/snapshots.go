package docs

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/aiviarium/rookery/internal/data"
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

	var latest int64
	if err := d.store.Pool().QueryRow(ctx,
		"SELECT coalesce(max(seq), 0) FROM rookery.document_updates WHERE document_id = $1", id).
		Scan(&latest); err != nil {
		return Snapshot{}, fmt.Errorf("docs: reading the document's sequence: %w", err)
	}
	if sequence > latest {
		return Snapshot{}, fmt.Errorf("%w: the document is at update %d, not %d", ErrInvalid, latest, sequence)
	}

	snapshot := Snapshot{
		DocumentID: id,
		Sequence:   sequence,
		Version:    version,
		Label:      label,
		CreatedBy:  principal.String(),
	}
	err := d.store.Pool().QueryRow(ctx, `
		INSERT INTO rookery.document_snapshots (document_id, seq, data, version, label, created_by)
		VALUES ($1, $2, $3, $4, $5, $6)
		ON CONFLICT (document_id, seq) DO UPDATE
		SET data = excluded.data, version = excluded.version, label = excluded.label, created_by = excluded.created_by
		RETURNING created_at`, id, sequence, data, version, label, snapshot.CreatedBy).
		Scan(&snapshot.At)
	if err != nil {
		return Snapshot{}, fmt.Errorf("docs: writing a snapshot: %w", err)
	}
	return snapshot, nil
}

// History lists a document's versions, oldest first. The caller needs `view`.
func (d *Documents) History(ctx context.Context, principal data.Principal, id string) ([]Snapshot, error) {
	if _, err := d.requireDocument(ctx, principal, id, readLevel); err != nil {
		return nil, err
	}

	rows, err := d.store.Pool().Query(ctx, `
		SELECT document_id, seq, version, coalesce(label, ''), created_by, created_at
		FROM rookery.document_snapshots
		WHERE document_id = $1 AND version
		ORDER BY seq`, id)
	if err != nil {
		return nil, fmt.Errorf("docs: reading the document's history: %w", err)
	}
	defer rows.Close()

	snapshots := make([]Snapshot, 0, 8)
	for rows.Next() {
		var snapshot Snapshot
		if err := rows.Scan(&snapshot.DocumentID, &snapshot.Sequence, &snapshot.Version, &snapshot.Label,
			&snapshot.CreatedBy, &snapshot.At); err != nil {
			return nil, fmt.Errorf("docs: reading the document's history: %w", err)
		}
		snapshot.At = snapshot.At.UTC()
		snapshots = append(snapshots, snapshot)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("docs: reading the document's history: %w", err)
	}
	return snapshots, nil
}

// latestSnapshot is the snapshot with the highest sequence, and its data. data is
// nil when the document has none.
func (d *Documents) latestSnapshot(ctx context.Context, id string) (int64, []byte, error) {
	var (
		sequence int64
		data     []byte
	)
	err := d.store.Pool().QueryRow(ctx, `
		SELECT seq, data FROM rookery.document_snapshots
		WHERE document_id = $1 ORDER BY seq DESC LIMIT 1`, id).Scan(&sequence, &data)
	if err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return 0, nil, nil
		}
		return 0, nil, fmt.Errorf("docs: reading the document's snapshot: %w", err)
	}
	return sequence, data, nil
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
