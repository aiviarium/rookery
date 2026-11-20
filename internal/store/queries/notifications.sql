-- name: CreateNotification :one
INSERT INTO rookery.notifications (id, user_id, title, body, link, resource, actions)
VALUES ($1, $2, $3, $4, $5, $6, $7)
RETURNING *;

-- name: GetNotification :one
SELECT *
FROM rookery.notifications
WHERE id = $1;

-- name: ListNotifications :many
SELECT *
FROM rookery.notifications
WHERE user_id = sqlc.arg('user_id')
  AND (sqlc.arg('unread_only')::boolean = false OR read_at IS NULL)
  AND id < sqlc.arg('cursor')::text COLLATE "C"
ORDER BY id DESC
LIMIT sqlc.arg('row_limit');

-- name: NotificationsAfter :many
SELECT *
FROM rookery.notifications
WHERE user_id = $1
  AND id > $2 COLLATE "C"
ORDER BY id
LIMIT $3;

-- name: MarkNotificationRead :one
UPDATE rookery.notifications
SET read_at = CASE WHEN sqlc.arg('read')::boolean THEN coalesce(read_at, now()) ELSE NULL END
WHERE id = sqlc.arg('id')
RETURNING *;

-- name: AnswerNotification :one
UPDATE rookery.notifications
SET answers = sqlc.arg('answers')
WHERE id = $1
RETURNING *;

-- name: SetNotificationPrefs :exec
INSERT INTO rookery.notification_prefs (user_id, prefs)
VALUES ($1, $2)
ON CONFLICT (user_id) DO UPDATE
SET prefs = excluded.prefs, updated_at = now();

-- name: GetNotificationPrefs :one
SELECT *
FROM rookery.notification_prefs
WHERE user_id = $1;

-- name: NewestNotification :one
SELECT coalesce(max(id), '')::text AS id
FROM rookery.notifications
WHERE user_id = $1;
