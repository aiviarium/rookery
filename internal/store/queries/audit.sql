-- name: InsertAudit :one
INSERT INTO rookery.audit (principal, procedure, resource, outcome, detail)
VALUES ($1, $2, $3, $4, $5)
RETURNING *;

-- name: ListAudit :many
SELECT *
FROM rookery.audit
WHERE (sqlc.narg('principal')::text IS NULL OR principal = sqlc.narg('principal'))
  AND (sqlc.narg('procedure')::text IS NULL OR procedure = sqlc.narg('procedure'))
  AND (sqlc.narg('resource')::text IS NULL OR resource = sqlc.narg('resource'))
  AND (sqlc.narg('after')::timestamptz IS NULL OR at >= sqlc.narg('after'))
  AND (sqlc.narg('before')::timestamptz IS NULL OR at <= sqlc.narg('before'))
  AND (sqlc.arg('cursor')::bigint = 0 OR id < sqlc.arg('cursor'))
ORDER BY id DESC
LIMIT sqlc.arg('row_limit');
