-- name: CreateProvisioner :one
INSERT INTO rookery.provisioners (name, type, plugin, args, node, remote)
VALUES ($1, $2, $3, $4, $5, $6)
RETURNING *;

-- name: UpdateProvisioner :one
UPDATE rookery.provisioners
SET args = $2, node = $3, remote = $4, updated_at = now()
WHERE name = $1
RETURNING *;

-- name: GetProvisioner :one
SELECT *
FROM rookery.provisioners
WHERE name = $1;

-- name: ListProvisioners :many
SELECT *
FROM rookery.provisioners
WHERE name > sqlc.arg('cursor')
ORDER BY name
LIMIT sqlc.arg('row_limit');

-- name: ListProvisionersVisibleTo :many
SELECT p.*
FROM rookery.provisioners p
WHERE p.name > sqlc.arg('cursor')
  AND EXISTS (
	SELECT 1
	FROM rookery.grants g
	WHERE g.resource = 'provisioner://' || p.name
	  AND g.principal = ANY(sqlc.arg('principals')::text[])
	  AND g.level >= 1
  )
ORDER BY p.name
LIMIT sqlc.arg('row_limit');

-- name: DeleteProvisioner :execrows
DELETE FROM rookery.provisioners
WHERE name = $1;

-- name: CreateProvisionedResource :one
INSERT INTO rookery.provisioned_resources (id, kind, name, provisioner, state, address)
VALUES ($1, $2, $3, $4, $5, $6)
RETURNING *;

-- name: GetProvisionedResource :one
SELECT *
FROM rookery.provisioned_resources
WHERE id = $1;

-- name: GetProvisionedMachine :one
SELECT *
FROM rookery.provisioned_resources
WHERE kind = 'machine'
  AND name = $1;

-- name: CountProvisionedResources :one
SELECT count(*)
FROM rookery.provisioned_resources
WHERE provisioner = $1;

-- name: ListProvisionedResources :many
SELECT *
FROM rookery.provisioned_resources
WHERE id > sqlc.arg('cursor')
  AND (sqlc.narg('provisioner')::text IS NULL OR provisioner = sqlc.narg('provisioner'))
  AND (sqlc.narg('kind')::text IS NULL OR kind = sqlc.narg('kind'))
ORDER BY id
LIMIT sqlc.arg('row_limit');

-- name: UpdateProvisionedResource :one
UPDATE rookery.provisioned_resources
SET state = $2, address = $3, updated_at = now()
WHERE id = $1
RETURNING *;

-- name: DeleteProvisionedResource :execrows
DELETE FROM rookery.provisioned_resources
WHERE id = $1;
