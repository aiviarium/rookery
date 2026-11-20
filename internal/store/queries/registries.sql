-- name: AddRegistry :execrows
INSERT INTO rookery.registries (name, url, public_key)
VALUES ($1, $2, $3)
ON CONFLICT (name) DO NOTHING;

-- name: ListRegistries :many
SELECT * FROM rookery.registries ORDER BY name;

-- name: DeleteRegistry :execrows
DELETE FROM rookery.registries WHERE name = $1;
