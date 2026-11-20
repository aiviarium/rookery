-- name: CreateDocument :one
INSERT INTO rookery.documents (id, type, parent, owner_id)
VALUES ($1, $2, $3, $4)
RETURNING *;

-- name: GetDocument :one
SELECT *
FROM rookery.documents
WHERE id = $1 AND deleted_at IS NULL;

-- name: ListDocuments :many
SELECT *
FROM rookery.documents
WHERE deleted_at IS NULL
  AND (sqlc.narg('parent')::text IS NULL OR parent = sqlc.narg('parent'))
ORDER BY updated_at DESC
LIMIT sqlc.arg('row_limit');

-- name: DeleteDocument :execrows
UPDATE rookery.documents
SET deleted_at = now(), updated_at = now()
WHERE id = $1 AND deleted_at IS NULL;

-- name: AdvanceDocument :one
UPDATE rookery.documents
SET head = head + 1, updated_at = now()
WHERE id = $1 AND deleted_at IS NULL
RETURNING head;

-- name: AppendDocumentUpdate :exec
INSERT INTO rookery.document_updates (document_id, seq, data)
VALUES ($1, $2, $3);

-- name: ListDocumentUpdates :many
SELECT seq, data
FROM rookery.document_updates
WHERE document_id = $1 AND seq > sqlc.arg('after') AND seq <= sqlc.arg('through')
ORDER BY seq;

-- name: PutDocumentSnapshot :one
INSERT INTO rookery.document_snapshots (document_id, seq, data, version, label, created_by)
SELECT $1, $2, $3, $4, $5, $6
FROM rookery.documents d
WHERE d.id = $1 AND d.deleted_at IS NULL AND $2 <= d.head
ON CONFLICT (document_id, seq) DO UPDATE
SET data = excluded.data, version = excluded.version, label = excluded.label,
    created_by = excluded.created_by, created_at = now()
WHERE NOT rookery.document_snapshots.version OR excluded.version
RETURNING *;

-- name: ListDocumentVersions :many
SELECT document_id, seq, version, label, created_by, created_at
FROM rookery.document_snapshots
WHERE document_id = $1 AND version
ORDER BY seq;

-- name: LatestDocumentSnapshot :one
SELECT seq, data
FROM rookery.document_snapshots
WHERE document_id = $1
ORDER BY seq DESC LIMIT 1;

-- name: PruneDocumentUpdates :execrows
DELETE FROM rookery.document_updates u
WHERE EXISTS (
    SELECT 1 FROM rookery.document_snapshots s
    WHERE s.document_id = u.document_id
      AND s.seq >= u.seq
      AND s.created_at < now() - interval '1 day'
);
