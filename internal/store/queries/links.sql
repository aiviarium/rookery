-- name: CreateLink :one
INSERT INTO rookery.links (token_hash, email, user_id, created_by, expires_at)
VALUES ($1, $2, sqlc.narg('user_id'), sqlc.narg('created_by'), $3)
RETURNING *;

-- name: GetLink :one
SELECT *
FROM rookery.links
WHERE token_hash = $1;

-- name: UseLink :execrows
UPDATE rookery.links
SET used_at = now()
WHERE token_hash = $1
  AND used_at IS NULL
  AND expires_at > now();

-- name: DeleteOldLinks :exec
DELETE FROM rookery.links
WHERE expires_at < now() - interval '30 days';
