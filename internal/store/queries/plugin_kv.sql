-- name: GetPluginKV :one
SELECT value, revision FROM rookery.plugin_kv WHERE plugin = $1 AND key = $2;

-- name: PutPluginKV :one
INSERT INTO rookery.plugin_kv (plugin, key, value, revision)
VALUES ($1, $2, $3, 1)
ON CONFLICT (plugin, key) DO UPDATE
SET value = excluded.value, revision = rookery.plugin_kv.revision + 1
RETURNING revision;

-- name: CreatePluginKV :one
INSERT INTO rookery.plugin_kv (plugin, key, value, revision)
VALUES ($1, $2, $3, 1)
ON CONFLICT (plugin, key) DO NOTHING
RETURNING revision;

-- name: CompareAndSetPluginKV :one
UPDATE rookery.plugin_kv
SET value = $3, revision = revision + 1
WHERE plugin = $1 AND key = $2 AND revision = $4
RETURNING revision;

-- name: DeletePluginKV :exec
DELETE FROM rookery.plugin_kv WHERE plugin = $1;
