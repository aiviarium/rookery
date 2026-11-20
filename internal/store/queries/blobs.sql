-- name: CreateBlob :one
INSERT INTO rookery.blobs (id, size, content_type, owner, resource)
VALUES ($1, $2, $3, $4, $5)
ON CONFLICT (id) DO NOTHING
RETURNING *;

-- name: GetBlob :one
SELECT *
FROM rookery.blobs
WHERE id = $1;

-- name: DeleteBlob :execrows
DELETE FROM rookery.blobs b
WHERE b.id = $1
  AND NOT EXISTS (SELECT 1 FROM rookery.refs r WHERE r.blob_id = b.id);

-- name: ListUnreferencedBlobs :many
SELECT *
FROM rookery.blobs b
WHERE NOT b.pinned
  AND coalesce(b.referenced_at, b.created_at) < now() - sqlc.arg('older')::interval
  AND NOT EXISTS (SELECT 1 FROM rookery.refs r WHERE r.blob_id = b.id)
ORDER BY b.created_at
LIMIT $1;

-- name: TouchBlob :exec
UPDATE rookery.blobs
SET referenced_at = now()
WHERE id = $1;

-- name: DeleteUnreferencedBlob :execrows
DELETE FROM rookery.blobs b
WHERE b.id = $1
  AND NOT b.pinned
  AND coalesce(b.referenced_at, b.created_at) < now() - sqlc.arg('older')::interval
  AND NOT EXISTS (SELECT 1 FROM rookery.refs r WHERE r.blob_id = b.id);

-- name: PinBlob :execrows
UPDATE rookery.blobs SET pinned = $2, referenced_at = now() WHERE id = $1;
