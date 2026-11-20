-- name: CreateConnection :one
INSERT INTO rookery.connections (id, type, name, values, key_id)
VALUES ($1, $2, $3, $4, $5)
RETURNING *;

-- name: GetConnection :one
SELECT *
FROM rookery.connections
WHERE id = $1;

-- name: GetConnectionByName :one
SELECT *
FROM rookery.connections
WHERE name = $1;

-- name: ListConnections :many
SELECT *
FROM rookery.connections
WHERE (sqlc.narg('type')::text IS NULL OR type = sqlc.narg('type'))
ORDER BY name;

-- name: UpdateConnection :one
UPDATE rookery.connections
SET name = $2, values = $3, key_id = $4, updated_at = now()
WHERE id = $1
RETURNING *;

-- name: DeleteConnection :execrows
DELETE FROM rookery.connections
WHERE id = $1;

-- name: ListConnectionsForRekey :many
SELECT *
FROM rookery.connections
ORDER BY id;

-- name: ResealConnection :exec
UPDATE rookery.connections
SET values = $2, key_id = $3
WHERE id = $1;
