-- name: CreateJob :one
INSERT INTO rookery.jobs (id, kind, args, resource, created_by, parent, state, max_attempts, host)
VALUES ($1, $2, $3, $4, $5, $6, 'queued', $7, $8)
RETURNING *;

-- name: GetJob :one
SELECT *
FROM rookery.jobs
WHERE id = $1;

-- name: ListJobs :many
SELECT *
FROM rookery.jobs
WHERE (sqlc.narg('kind')::text IS NULL OR kind = sqlc.narg('kind'))
  AND (sqlc.narg('state')::text IS NULL OR state = sqlc.narg('state'))
  AND (sqlc.narg('resource')::text IS NULL OR resource = sqlc.narg('resource'))
  AND id < sqlc.arg('cursor')::text COLLATE "C"
ORDER BY id DESC
LIMIT sqlc.arg('row_limit');

-- name: ClaimCoreJob :one
WITH candidate AS (
	SELECT id
	FROM rookery.jobs
	WHERE state = 'queued'
	  AND available_at <= now()
	  AND kind = ANY(sqlc.arg('kinds')::text[])
	  AND NOT (host <> '' AND host = ANY(sqlc.arg('busy_hosts')::text[]))
	ORDER BY available_at, id
	LIMIT 1
	FOR UPDATE SKIP LOCKED
)
UPDATE rookery.jobs
SET state = 'running',
    attempts = attempts + 1,
    started_at = coalesce(started_at, now()),
    cancel_requested = false
FROM candidate
WHERE jobs.id = candidate.id
RETURNING jobs.*;

-- name: ClaimPluginJob :one
WITH candidate AS (
	SELECT id
	FROM rookery.jobs
	WHERE state = 'queued'
	  AND available_at <= now()
	  AND kind = ANY(sqlc.arg('kinds')::text[])
	ORDER BY available_at, id
	LIMIT 1
	FOR UPDATE SKIP LOCKED
)
UPDATE rookery.jobs
SET state = 'running',
    attempts = attempts + 1,
    started_at = coalesce(started_at, now()),
    claim = sqlc.arg('claim'),
    heartbeat_at = now()
FROM candidate
WHERE jobs.id = candidate.id
RETURNING jobs.*;

-- name: HeartbeatJob :execrows
UPDATE rookery.jobs
SET heartbeat_at = now()
WHERE id = $1
  AND claim = $2
  AND state = 'running';

-- name: CompleteJob :execrows
UPDATE rookery.jobs
SET state = 'succeeded',
    result = sqlc.narg('result'),
    claim = NULL,
    heartbeat_at = NULL,
    finished_at = now()
WHERE id = sqlc.arg('id')
  AND state = 'running'
  AND (sqlc.narg('claim')::text IS NULL OR claim = sqlc.narg('claim'));

-- name: RetryJob :execrows
UPDATE rookery.jobs
SET state = 'queued',
    error = sqlc.arg('error'),
    claim = NULL,
    heartbeat_at = NULL,
    available_at = now() + sqlc.arg('backoff')::interval,
    cancel_requested = false
WHERE id = sqlc.arg('id')
  AND state = 'running'
  AND (sqlc.narg('claim')::text IS NULL OR claim = sqlc.narg('claim'));

-- name: FailJob :execrows
UPDATE rookery.jobs
SET state = 'failed',
    error = sqlc.arg('error'),
    claim = NULL,
    heartbeat_at = NULL,
    finished_at = now()
WHERE id = sqlc.arg('id')
  AND state = 'running'
  AND (sqlc.narg('claim')::text IS NULL OR claim = sqlc.narg('claim'));

-- name: EndJob :execrows
UPDATE rookery.jobs
SET state = sqlc.arg('state'),
    error = sqlc.arg('error'),
    claim = NULL,
    heartbeat_at = NULL,
    finished_at = now()
WHERE id = sqlc.arg('id')
  AND state = 'running';

-- name: CancelQueuedJob :execrows
UPDATE rookery.jobs
SET state = 'cancelled',
    finished_at = now()
WHERE id = $1
  AND state = 'queued';

-- name: RequestCancel :execrows
UPDATE rookery.jobs
SET cancel_requested = true
WHERE id = $1
  AND state = 'running';

-- name: CancelRequested :one
SELECT cancel_requested
FROM rookery.jobs
WHERE id = $1;

-- name: RequeueJob :one
UPDATE rookery.jobs
SET state = 'queued',
    attempts = 0,
    error = '',
    result = NULL,
    claim = NULL,
    heartbeat_at = NULL,
    available_at = now(),
    started_at = NULL,
    finished_at = NULL,
    cancel_requested = false
WHERE id = $1
RETURNING *;

-- name: ReturnExpiredClaims :execrows
UPDATE rookery.jobs
SET state = 'queued',
    claim = NULL,
    heartbeat_at = NULL,
    available_at = now()
WHERE state = 'running'
  AND claim IS NOT NULL
  AND heartbeat_at < now() - sqlc.arg('after')::interval;

-- name: InsertJobLine :one
INSERT INTO rookery.job_lines (job_id, line)
VALUES ($1, $2)
RETURNING *;

-- name: LinesAfter :many
SELECT *
FROM rookery.job_lines
WHERE job_id = $1
  AND id > $2
ORDER BY id
LIMIT $3;

-- name: InterruptRunningJobs :execrows
UPDATE rookery.jobs
SET state = 'interrupted',
    error = 'interrupted by the server stopping',
    claim = NULL,
    heartbeat_at = NULL,
    finished_at = now()
WHERE state = 'running';

-- name: DeleteOldJobs :execrows
DELETE FROM rookery.jobs
WHERE finished_at < now() - sqlc.arg('before')::interval;
