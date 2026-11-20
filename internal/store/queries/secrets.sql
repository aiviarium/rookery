-- name: UpsertSecret :one
INSERT INTO rookery.secrets (name, scope, value, key_id)
VALUES ($1, $2, $3, $4)
ON CONFLICT (name, scope) DO UPDATE
SET value = excluded.value, key_id = excluded.key_id, updated_at = now()
RETURNING *;

-- name: GetSecret :one
SELECT *
FROM rookery.secrets
WHERE name = $1
  AND scope = $2;

-- name: ListSecrets :many
SELECT *
FROM rookery.secrets
WHERE (sqlc.narg('scope')::text IS NULL OR scope = sqlc.narg('scope'))
ORDER BY scope, name;

-- name: DeleteSecret :execrows
DELETE FROM rookery.secrets
WHERE name = $1
  AND scope = $2;

-- name: RenameSecret :execrows
UPDATE rookery.secrets
SET name = $3, updated_at = now()
WHERE name = $1
  AND scope = $2;

-- name: ListSecretsForRekey :many
SELECT *
FROM rookery.secrets
ORDER BY name, scope FOR UPDATE;

-- name: ResealSecret :exec
UPDATE rookery.secrets
SET value = $3, key_id = $4
WHERE name = $1
  AND scope = $2;

-- name: LockSecret :one
SELECT * FROM rookery.secrets WHERE name = $1 AND scope = $2 FOR UPDATE;

-- name: RenameSealedSecret :one
UPDATE rookery.secrets
SET name = sqlc.arg('new_name'), value = sqlc.arg('sealed_value'),
    key_id = sqlc.arg('new_key_id'), updated_at = now()
WHERE name = sqlc.arg('old_name') AND scope = sqlc.arg('scope')
RETURNING *;
