-- name: EnsureTopic :exec
INSERT INTO rookery.topics (name)
VALUES ($1)
ON CONFLICT DO NOTHING;

-- name: InsertTopicMessage :one
INSERT INTO rookery.topic_messages (topic, payload)
VALUES ($1, $2)
RETURNING *;

-- name: TopicMessagesAfter :many
SELECT *
FROM rookery.topic_messages
WHERE topic = $1
  AND id >= $2
  AND available_at <= now()
ORDER BY id
LIMIT $3;

-- name: TopicMessage :one
SELECT *
FROM rookery.topic_messages
WHERE id = $1;

-- name: CursorFor :one
INSERT INTO rookery.topic_cursors (topic, consumer)
VALUES ($1, $2)
ON CONFLICT (topic, consumer) DO UPDATE
SET updated_at = now()
RETURNING *;

-- name: AdvanceCursor :exec
UPDATE rookery.topic_cursors
SET next_seq = GREATEST(next_seq, $3), updated_at = now()
WHERE topic = $1
  AND consumer = $2;

-- name: DelayMessage :exec
UPDATE rookery.topic_messages
SET attempts = attempts + 1, available_at = now() + sqlc.arg('delay')::interval
WHERE id = sqlc.arg('id');

-- name: DeleteTopicMessage :execrows
DELETE FROM rookery.topic_messages
WHERE id = $1;

-- name: InsertDeadLetter :one
INSERT INTO rookery.topic_dead (topic, consumer, payload, attempts, error)
VALUES ($1, $2, $3, $4, $5)
RETURNING *;

-- name: ListDeadLetters :many
SELECT *
FROM rookery.topic_dead
WHERE (sqlc.narg('topic')::text IS NULL OR topic = sqlc.narg('topic'))
  AND (sqlc.narg('consumer')::text IS NULL OR consumer = sqlc.narg('consumer'))
ORDER BY id DESC
LIMIT sqlc.arg('row_limit');

-- name: GetDeadLetter :one
SELECT *
FROM rookery.topic_dead
WHERE id = $1;

-- name: DeleteDeadLetter :execrows
DELETE FROM rookery.topic_dead
WHERE id = $1;

-- name: TopicMessagesForInspect :many
SELECT *
FROM rookery.topic_messages
WHERE topic = $1
ORDER BY id DESC
LIMIT $2;

-- name: RedeliverTopicMessage :one
INSERT INTO rookery.topic_messages (topic, payload, attempts, available_at)
VALUES ($1, $2, $3, now() + sqlc.arg('delay')::interval)
RETURNING *;
