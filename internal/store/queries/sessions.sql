-- name: CreateSession :one
INSERT INTO rookery.sessions (id, secret_hash, user_id, csrf, user_agent)
VALUES ($1, $2, $3, $4, $5)
RETURNING *;

-- name: GetSessionByHash :one
SELECT *
FROM rookery.sessions
WHERE secret_hash = $1;

-- name: TouchSession :exec
UPDATE rookery.sessions
SET seen_at = now()
WHERE id = $1;

-- name: ListSessions :many
SELECT *
FROM rookery.sessions
WHERE user_id = $1
ORDER BY created_at DESC;

-- name: DeleteSession :execrows
DELETE FROM rookery.sessions
WHERE id = $1
  AND user_id = $2;

-- name: DeleteOtherSessions :exec
DELETE FROM rookery.sessions
WHERE user_id = $1
  AND id <> $2;
