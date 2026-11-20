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

-- name: PluginProvisionerUser :one
SELECT name FROM rookery.provisioners WHERE plugin = $1 ORDER BY name LIMIT 1;

-- name: PluginHostProfiles :many
SELECT name, profile FROM rookery.hosts ORDER BY name;

-- name: PutPluginArchive :exec
INSERT INTO rookery.plugin_archives (plugin, archive) VALUES ($1, $2)
ON CONFLICT (plugin) DO UPDATE SET archive = excluded.archive;

-- name: GetPluginArchive :one
SELECT archive FROM rookery.plugin_archives WHERE plugin = $1;

-- name: PutPluginDeclaration :exec
INSERT INTO rookery.plugin_declarations (plugin, manifest) VALUES ($1, $2)
ON CONFLICT (plugin) DO UPDATE SET manifest = excluded.manifest;

-- name: GetPluginDeclaration :one
SELECT manifest FROM rookery.plugin_declarations WHERE plugin = $1;

-- name: DeletePluginDeclaration :exec
DELETE FROM rookery.plugin_declarations WHERE plugin = $1;

-- name: PluginsWithQueuedJobs :many
SELECT p.id FROM rookery.plugins p
WHERE p.enabled AND EXISTS (
    SELECT 1 FROM rookery.jobs j
    WHERE j.state = 'queued' AND j.available_at <= now()
      AND split_part(j.kind, '/', 1) = p.id AND strpos(j.kind, '/') > 0
)
ORDER BY p.id;
