-- name: GetSetting :one
SELECT *
FROM rookery.settings
WHERE key = $1
  AND scope = $2
  AND scope_id = $3;

-- name: ListSettingValues :many
SELECT *
FROM rookery.settings
WHERE key = $1;

-- name: SetSetting :exec
INSERT INTO rookery.settings (key, scope, scope_id, value)
VALUES ($1, $2, $3, $4)
ON CONFLICT (key, scope, scope_id) DO UPDATE
SET value = excluded.value, updated_at = now();

-- name: DeleteSetting :execrows
DELETE FROM rookery.settings
WHERE key = $1
  AND scope = $2
  AND scope_id = $3;

-- name: ListSettings :many
SELECT DISTINCT key
FROM rookery.settings
ORDER BY key;
