-- name: AcquireLease :one
INSERT INTO rookery.leases (name, holder, fencing, expires_at)
VALUES ($1, $2, 1, now() + sqlc.arg('ttl')::interval)
ON CONFLICT (name) DO UPDATE
SET holder = excluded.holder,
    fencing = rookery.leases.fencing + 1,
    expires_at = excluded.expires_at,
    updated_at = now()
WHERE rookery.leases.expires_at < now()
   OR rookery.leases.holder = excluded.holder
RETURNING *;

-- name: GetLease :one
SELECT *
FROM rookery.leases
WHERE name = $1;

-- name: RenewLease :execrows
UPDATE rookery.leases
SET expires_at = now() + sqlc.arg('ttl')::interval, updated_at = now()
WHERE name = sqlc.arg('name')
  AND holder = sqlc.arg('holder')
  AND fencing = sqlc.arg('fencing');

-- name: ReleaseLease :execrows
DELETE FROM rookery.leases
WHERE name = $1
  AND holder = $2
  AND fencing = $3;

-- name: ExpiredLeases :many
SELECT *
FROM rookery.leases
WHERE expires_at < now();
