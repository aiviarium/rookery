-- name: CreateHost :one
INSERT INTO rookery.hosts (name, type, profile, address, created_by)
VALUES ($1, $2, $3, $4, sqlc.narg('created_by'))
RETURNING *;

-- name: GetHost :one
SELECT *
FROM rookery.hosts
WHERE name = $1;

-- name: ListHosts :many
SELECT *
FROM rookery.hosts
ORDER BY name;

-- name: RenameHost :one
UPDATE rookery.hosts
SET name = sqlc.arg('new_name'),
    updated_at = now()
WHERE name = sqlc.arg('name')
RETURNING *;

-- name: DeleteHost :execrows
DELETE FROM rookery.hosts
WHERE name = $1;

-- name: SetHostProfile :one
UPDATE rookery.hosts
SET profile = sqlc.arg('profile'),
    type = sqlc.arg('type'),
    updated_at = now()
WHERE name = sqlc.arg('name')
RETURNING *;

-- name: SetHostApplied :one
UPDATE rookery.hosts
SET applied_digest = sqlc.arg('digest'),
    applied_at = now(),
    type = sqlc.arg('type'),
    address = sqlc.arg('address'),
    last_seen_at = now(),
    updated_at = now()
WHERE name = sqlc.arg('name')
RETURNING *;

-- name: SetHostAddress :one
UPDATE rookery.hosts
SET address = $2,
    updated_at = now()
WHERE name = $1
RETURNING *;

-- name: TouchHost :exec
UPDATE rookery.hosts
SET last_seen_at = now()
WHERE name = $1;

-- name: UpsertHostLabel :exec
INSERT INTO rookery.host_labels (host, key, value)
VALUES ($1, $2, $3)
ON CONFLICT (host, key) DO UPDATE
SET value = excluded.value;

-- name: DeleteHostLabel :exec
DELETE FROM rookery.host_labels
WHERE host = $1
  AND key = $2;

-- name: ListHostLabels :many
SELECT *
FROM rookery.host_labels
WHERE host = $1
ORDER BY key;

-- name: GetHostCursor :one
SELECT *
FROM rookery.host_cursors
WHERE host = $1;

-- name: SetHostCursor :exec
INSERT INTO rookery.host_cursors (host, sequence)
VALUES ($1, $2)
ON CONFLICT (host) DO UPDATE
SET sequence = excluded.sequence,
    updated_at = now();

-- name: DeleteHostCursor :exec
DELETE FROM rookery.host_cursors
WHERE host = $1;
