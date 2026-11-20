-- Roll-outs. The service plans, starts and steers them; the advance loop reads
-- the ones that can move, and their hosts.

-- name: CreateRollout :one
INSERT INTO rookery.rollouts (id, kind, args, resource, created_by, state, selection, strategy)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
RETURNING *;

-- name: GetRollout :one
SELECT *
FROM rookery.rollouts
WHERE id = $1;

-- name: ListRollouts :many
SELECT *
FROM rookery.rollouts
WHERE id < $1
  AND (sqlc.narg('state')::text IS NULL OR state = sqlc.narg('state'))
ORDER BY id DESC
LIMIT $2;

-- name: ListRunnableRollouts :many
SELECT *
FROM rookery.rollouts
WHERE state = 'running'
  AND (paused_until IS NULL OR paused_until <= now())
ORDER BY id
LIMIT $1;

-- name: StartRollout :one
UPDATE rookery.rollouts
SET state = 'running',
    paused_until = NULL,
    paused_because = '',
    started_at = COALESCE(started_at, now())
WHERE id = $1
RETURNING *;

-- name: SetRolloutPause :one
UPDATE rookery.rollouts
SET paused_until = $2
WHERE id = $1
RETURNING *;

-- name: PauseRollout :one
UPDATE rookery.rollouts
SET state = 'paused',
    paused_until = NULL,
    paused_because = $2
WHERE id = $1
RETURNING *;

-- name: PauseRunningRollouts :execrows
UPDATE rookery.rollouts
SET state = 'paused',
    paused_until = NULL,
    paused_because = 'the server changed leader'
WHERE state = 'running';

-- name: ResumeRollout :one
UPDATE rookery.rollouts
SET state = 'running',
    paused_until = NULL,
    paused_because = '',
    canary_done = true,
    started_at = COALESCE(started_at, now())
WHERE id = $1
RETURNING *;

-- name: FinishRollout :one
UPDATE rookery.rollouts
SET state = $2,
    paused_until = NULL,
    paused_because = '',
    finished_at = now()
WHERE id = $1
RETURNING *;

-- name: CreateRolloutHost :exec
INSERT INTO rookery.rollout_hosts (rollout_id, host, batch, state, reason)
VALUES ($1, $2, $3, $4, $5)
ON CONFLICT (rollout_id, host) DO NOTHING;

-- name: ListRolloutHosts :many
SELECT *
FROM rookery.rollout_hosts
WHERE rollout_id = $1
ORDER BY batch, host;

-- name: RunningRolloutHosts :many
SELECT *
FROM rookery.rollout_hosts
WHERE rollout_id = $1
  AND state = 'running'
ORDER BY host;

-- name: NextRolloutBatch :many
SELECT h.*
FROM rookery.rollout_hosts h
WHERE h.rollout_id = $1
  AND h.state = 'pending'
  AND h.batch = (
    SELECT min(want.batch)
    FROM rookery.rollout_hosts want
    WHERE want.rollout_id = $1 AND want.state = 'pending'
  )
ORDER BY h.host;

-- name: StartRolloutHost :one
UPDATE rookery.rollout_hosts
SET state = 'running',
    job = $2,
    started_at = now()
WHERE rollout_id = $1
  AND host = $3
  AND state = 'pending'
RETURNING *;

-- name: EndRolloutHost :one
UPDATE rookery.rollout_hosts
SET state = $2,
    error = $3,
    finished_at = now()
WHERE rollout_id = $1
  AND host = $4
RETURNING *;

-- name: CancelPendingRolloutHosts :execrows
UPDATE rookery.rollout_hosts
SET state = 'cancelled',
    finished_at = now()
WHERE rollout_id = $1
  AND state = 'pending';

-- name: ListRolloutHostsIn :many
SELECT *
FROM rookery.rollout_hosts
WHERE rollout_id = ANY(sqlc.arg('rollouts')::text[])
ORDER BY rollout_id, batch, host;
