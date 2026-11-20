-- name: CreateUser :one
INSERT INTO rookery.users (id, email, name, admin, disabled)
VALUES ($1, $2, $3, $4, $5)
RETURNING *;

-- name: GetUser :one
SELECT *
FROM rookery.users
WHERE id = $1;

-- name: GetUserByEmail :one
SELECT *
FROM rookery.users
WHERE email = $1;

-- name: ListUsers :many
SELECT *
FROM rookery.users
WHERE id > $1
ORDER BY id
LIMIT $2;

-- name: SearchUsers :many
SELECT *
FROM rookery.users
WHERE (email ILIKE $1 OR name ILIKE $1)
  AND id > $2
ORDER BY id
LIMIT $3;

-- name: SetUserFlags :one
UPDATE rookery.users
SET admin = $2, disabled = $3
WHERE id = $1
RETURNING *;

-- name: CountAdmins :one
SELECT count(*)
FROM rookery.users
WHERE admin AND NOT disabled;
