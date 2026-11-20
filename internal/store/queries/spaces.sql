-- name: CreateSpace :one
INSERT INTO rookery.spaces (id, kind, name, created_by)
VALUES ($1, $2, $3, sqlc.narg('created_by'))
RETURNING *;

-- name: GetSpace :one
SELECT *
FROM rookery.spaces
WHERE id = $1;

-- name: ListSpaces :many
SELECT *
FROM rookery.spaces
WHERE id > $1
  AND (sqlc.narg('kind')::text IS NULL OR kind = sqlc.narg('kind'))
  AND (sqlc.arg('include_archived')::boolean OR NOT archived)
ORDER BY id
LIMIT sqlc.arg('row_limit');

-- name: RenameSpace :one
UPDATE rookery.spaces
SET name = $2, updated_at = now()
WHERE id = $1
RETURNING *;

-- name: ArchiveSpace :one
UPDATE rookery.spaces
SET archived = $2, updated_at = now()
WHERE id = $1
RETURNING *;

-- name: DeleteSpace :execrows
DELETE FROM rookery.spaces
WHERE id = $1;

-- name: ListSpacesVisibleTo :many
SELECT s.*
FROM rookery.spaces s
WHERE s.id > sqlc.arg('cursor')
  AND (sqlc.narg('kind')::text IS NULL OR s.kind = sqlc.narg('kind'))
  AND (sqlc.arg('include_archived')::boolean OR NOT s.archived)
  AND EXISTS (
	SELECT 1
	FROM rookery.grants g
	WHERE g.resource = 'space://' || s.id
	  AND g.principal = ANY(sqlc.arg('principals')::text[])
	  AND g.level >= 1
  )
ORDER BY s.id
LIMIT sqlc.arg('row_limit');
