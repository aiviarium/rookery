-- name: UpsertResource :exec
INSERT INTO rookery.resources (uri, parent)
VALUES ($1, sqlc.narg('parent'))
ON CONFLICT (uri) DO UPDATE
SET parent = COALESCE(excluded.parent, rookery.resources.parent);

-- name: GetResource :one
SELECT *
FROM rookery.resources
WHERE uri = $1;

-- name: DeleteResource :execrows
DELETE FROM rookery.resources
WHERE uri = $1;

-- name: UpsertGrant :one
INSERT INTO rookery.grants (resource, principal, level)
VALUES ($1, $2, $3)
ON CONFLICT (resource, principal) DO UPDATE
SET level = excluded.level
RETURNING *;

-- name: DeleteGrant :execrows
DELETE FROM rookery.grants
WHERE resource = $1
  AND principal = $2;

-- name: ListGrantsForResource :many
SELECT *
FROM rookery.grants
WHERE resource = $1
ORDER BY principal;

-- name: ListGrants :many
SELECT *
FROM rookery.grants
WHERE resource > sqlc.arg('after_resource')::text
   OR (resource = sqlc.arg('after_resource')::text AND principal > sqlc.arg('after_principal')::text)
ORDER BY resource, principal
LIMIT sqlc.arg('row_limit');

-- name: HeldLevel :one
WITH RECURSIVE chain AS (
	SELECT r0.uri, r0.parent
	FROM rookery.resources r0
	WHERE r0.uri = $1
	UNION ALL
	SELECT r.uri, r.parent
	FROM rookery.resources r
	JOIN chain c ON r.uri = c.parent
)
SELECT COALESCE(max(g.level), 0)::int AS level
FROM rookery.grants g
JOIN chain c ON g.resource = c.uri
WHERE g.principal = ANY($2::text[]);

-- name: DeleteGrantsForResource :exec
DELETE FROM rookery.grants
WHERE resource = $1;
