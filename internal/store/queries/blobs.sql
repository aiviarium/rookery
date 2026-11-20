-- name: CreateBlob :one
INSERT INTO rookery.blobs (id, size, content_type, owner, resource)
VALUES ($1, $2, $3, $4, $5)
ON CONFLICT (id) DO UPDATE
SET content_type = excluded.content_type,
    owner = CASE WHEN rookery.blobs.owner = '' THEN excluded.owner ELSE rookery.blobs.owner END,
    resource = CASE WHEN rookery.blobs.resource = '' THEN excluded.resource ELSE rookery.blobs.resource END,
    referenced_at = now()
RETURNING *;

-- name: GetBlob :one
SELECT *
FROM rookery.blobs
WHERE id = $1;

-- name: DeleteBlob :execrows
DELETE FROM rookery.blobs
WHERE id = $1;

-- name: ListUnreferencedBlobs :many
SELECT *
FROM rookery.blobs
WHERE NOT pinned
  AND (referenced_at IS NULL OR referenced_at < now() - sqlc.arg('older')::interval)
ORDER BY created_at
LIMIT $1;

-- name: TouchBlob :exec
UPDATE rookery.blobs
SET referenced_at = now()
WHERE id = $1;
