-- name: GetTailnetConfig :one
SELECT control_url, tag, api_key, key_id FROM rookery.tailnet_config WHERE singleton;

-- name: SetTailnetConfig :exec
INSERT INTO rookery.tailnet_config (control_url, tag, api_key, key_id)
VALUES ($1, $2, $3, $4)
ON CONFLICT (singleton) DO UPDATE
SET control_url = excluded.control_url, tag = excluded.tag,
    api_key = excluded.api_key, key_id = excluded.key_id;

-- name: GetTailnetState :one
SELECT value FROM rookery.tailnet_state WHERE key = $1;

-- name: PutTailnetState :exec
INSERT INTO rookery.tailnet_state (key, value) VALUES ($1, $2)
ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = now();

-- name: DeleteTailnetState :exec
DELETE FROM rookery.tailnet_state WHERE key = $1;

-- name: ResealTailnetKey :execrows
UPDATE rookery.tailnet_config SET api_key = $1, key_id = $2
WHERE singleton AND key_id = sqlc.arg('old_key_id');
