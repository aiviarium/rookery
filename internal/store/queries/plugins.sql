-- Plugins. The installer writes the rows; the service reads them.

-- name: UpsertPlugin :one
INSERT INTO rookery.plugins (id, name, version, sha256, manifest, enabled)
VALUES ($1, $2, $3, $4, $5, $6)
ON CONFLICT (id) DO UPDATE
SET name = excluded.name,
    version = excluded.version,
    sha256 = excluded.sha256,
    manifest = excluded.manifest,
    enabled = excluded.enabled,
    updated_at = now()
RETURNING *;

-- name: GetPlugin :one
SELECT *
FROM rookery.plugins
WHERE id = $1;

-- name: ListPlugins :many
SELECT *
FROM rookery.plugins
ORDER BY id;

-- name: ListEnabledPlugins :many
SELECT *
FROM rookery.plugins
WHERE enabled
ORDER BY id;

-- name: SetPluginEnabled :one
UPDATE rookery.plugins
SET enabled = $2,
    updated_at = now()
WHERE id = $1
RETURNING *;

-- name: DeletePlugin :execrows
DELETE FROM rookery.plugins
WHERE id = $1;

-- name: DeleteSchedulesNamed :execrows
DELETE FROM rookery.schedules
WHERE name LIKE sqlc.arg('prefix')::text;
