-- name: UpsertPluginEntity :exec
INSERT INTO rookery.entities (uri, kind, title, body, access, embedding)
VALUES ($1, $2, $3, $4, $5, sqlc.narg('embedding')::text::vector)
ON CONFLICT (uri) DO UPDATE SET
    kind = excluded.kind, title = excluded.title, body = excluded.body,
    access = excluded.access, embedding = excluded.embedding, updated_at = now();

-- name: DeletePluginEntities :exec
DELETE FROM rookery.entities WHERE starts_with(uri, $1);
