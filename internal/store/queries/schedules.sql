-- name: CreateSchedule :one
INSERT INTO rookery.schedules (id, name, kind, args, resource, cron, timezone, enabled, next_run_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
RETURNING *;

-- name: GetSchedule :one
SELECT *
FROM rookery.schedules
WHERE id = $1;

-- name: ListSchedules :many
SELECT *
FROM rookery.schedules
WHERE id > $1
ORDER BY id
LIMIT $2;

-- name: ListDueSchedules :many
SELECT *
FROM rookery.schedules
WHERE enabled
  AND next_run_at IS NOT NULL
  AND next_run_at <= now()
ORDER BY next_run_at
LIMIT $1;

-- name: UpdateScheduleRun :exec
UPDATE rookery.schedules
SET last_run_at = now(), next_run_at = $2
WHERE id = $1;

-- name: SetScheduleNextRun :exec
UPDATE rookery.schedules
SET next_run_at = $2
WHERE id = $1;

-- name: UpdateScheduleFields :one
UPDATE rookery.schedules
SET name = sqlc.arg('name'),
    kind = sqlc.arg('kind'),
    args = sqlc.arg('args'),
    resource = sqlc.arg('resource'),
    cron = sqlc.arg('cron'),
    timezone = sqlc.arg('timezone'),
    enabled = sqlc.arg('enabled'),
    next_run_at = sqlc.narg('next_run_at')
WHERE id = sqlc.arg('id')
RETURNING *;

-- name: DeleteSchedule :execrows
DELETE FROM rookery.schedules
WHERE id = $1;
