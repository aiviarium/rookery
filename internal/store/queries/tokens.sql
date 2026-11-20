-- name: CreateToken :one
INSERT INTO rookery.tokens (id, user_id, name, token_hash)
VALUES ($1, $2, $3, $4)
RETURNING *;

-- name: GetTokenByHash :one
SELECT *
FROM rookery.tokens
WHERE token_hash = $1;

-- name: TouchToken :exec
UPDATE rookery.tokens
SET last_used_at = now()
WHERE id = $1;

-- name: ListTokens :many
SELECT *
FROM rookery.tokens
WHERE user_id = $1
ORDER BY created_at DESC;

-- name: DeleteToken :execrows
DELETE FROM rookery.tokens
WHERE id = $1
  AND user_id = $2;
