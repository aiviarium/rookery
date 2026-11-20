-- name: CreateSSHKey :one
INSERT INTO rookery.keys (id, name, key_type, public_key, private_key, key_id, fingerprint)
VALUES ($1, $2, $3, $4, $5, $6, $7)
RETURNING *;

-- name: GetActiveSSHKey :one
SELECT *
FROM rookery.keys
WHERE retired_at IS NULL
ORDER BY created_at DESC
LIMIT 1;

-- name: GetSSHKey :one
SELECT *
FROM rookery.keys
WHERE id = $1;

-- name: ListSSHKeys :many
SELECT *
FROM rookery.keys
ORDER BY created_at DESC;

-- name: RetireSSHKey :exec
UPDATE rookery.keys
SET retired_at = now()
WHERE id = $1
  AND retired_at IS NULL;

-- name: GetKnownHost :one
SELECT *
FROM rookery.known_hosts
WHERE address = $1;

-- name: ListKnownHosts :many
SELECT *
FROM rookery.known_hosts
ORDER BY address;

-- name: CreateKnownHost :one
INSERT INTO rookery.known_hosts (address, host, key_type, public_key, fingerprint)
VALUES ($1, $2, $3, $4, $5)
RETURNING *;

-- name: OfferKnownHostKey :exec
UPDATE rookery.known_hosts
SET pending_key_type = sqlc.arg('key_type'),
    pending_public_key = sqlc.arg('public_key'),
    pending_fingerprint = sqlc.arg('fingerprint'),
    pending_at = now()
WHERE address = sqlc.arg('address');

-- name: TrustKnownHostKey :one
UPDATE rookery.known_hosts
SET key_type = COALESCE(pending_key_type, key_type),
    public_key = COALESCE(pending_public_key, public_key),
    fingerprint = COALESCE(pending_fingerprint, fingerprint),
    pending_key_type = NULL,
    pending_public_key = NULL,
    pending_fingerprint = NULL,
    pending_at = NULL,
    trusted_at = now()
WHERE address = $1
RETURNING *;

-- name: DeleteKnownHost :execrows
DELETE FROM rookery.known_hosts
WHERE address = $1;

-- name: TrustKnownHostPending :one
UPDATE rookery.known_hosts
SET key_type = pending_key_type,
    public_key = pending_public_key,
    fingerprint = pending_fingerprint,
    pending_key_type = NULL,
    pending_public_key = NULL,
    pending_fingerprint = NULL,
    pending_at = NULL,
    trusted_at = now()
WHERE address = sqlc.arg('address')
  AND pending_fingerprint = sqlc.arg('fingerprint')
RETURNING *;

-- name: SetKnownHostHost :exec
UPDATE rookery.known_hosts
SET host = $2
WHERE address = $1;
