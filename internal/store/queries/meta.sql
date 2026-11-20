-- name: GetMeta :one
SELECT value
FROM rookery.meta
WHERE key = $1;

-- name: SetMeta :exec
INSERT INTO rookery.meta (key, value)
VALUES ($1, $2)
ON CONFLICT (key) DO UPDATE
SET value = excluded.value, updated_at = now();
