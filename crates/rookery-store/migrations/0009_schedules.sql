-- Schedules that enqueue jobs, by cron expression in a time zone or by interval, and every delivery to
-- /hooks/<plugin>/<name>.

CREATE TABLE rookery.schedules (
    id            uuid PRIMARY KEY,
    -- The plugin it is of, or rookery for one of the server's own kinds of job.
    plugin        text NOT NULL,
    name          text NOT NULL,
    title         text,
    -- The kind of job it enqueues, such as apply or <plugin>/<job>, its params and the resource it is
    -- about, whose access the job's follows.
    kind          text NOT NULL,
    params        jsonb NOT NULL DEFAULT '{}',
    subject       text,
    cron          text,
    interval_secs bigint,
    timezone      text NOT NULL DEFAULT 'UTC',
    -- What happens to runs missed while no server ran it: once fires one, skip none, all each.
    missed        text NOT NULL DEFAULT 'once' CHECK (missed IN ('once', 'skip', 'all')),
    -- Who enqueues its jobs, and the person a plugin made it for.
    principal     text NOT NULL,
    on_behalf_of  uuid REFERENCES rookery.users (id) ON DELETE CASCADE,
    space_id      uuid,
    enabled       boolean NOT NULL DEFAULT true,
    -- Declared in its plugin's manifest, rather than made while the server runs.
    declared      boolean NOT NULL DEFAULT false,
    next_run_at   timestamptz,
    last_run_at   timestamptz,
    last_job_id   uuid,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    CHECK ((cron IS NULL) <> (interval_secs IS NULL)),
    CONSTRAINT schedules_key UNIQUE NULLS NOT DISTINCT (plugin, name, space_id, on_behalf_of)
);

CREATE INDEX schedules_due ON rookery.schedules (next_run_at) WHERE enabled;

-- Every request to /hooks/<plugin>/<name>, accepted or not.
CREATE TABLE rookery.webhook_deliveries (
    id          uuid PRIMARY KEY,
    plugin      text NOT NULL,
    hook        text NOT NULL,
    outcome     text NOT NULL CHECK (outcome IN ('accepted', 'duplicate', 'refused')),
    status      integer NOT NULL,
    reason      text,
    -- The delivery id or signature replays are told by.
    replay_key  text,
    job_id      uuid,
    bytes       integer NOT NULL DEFAULT 0,
    received_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX webhook_deliveries_hook ON rookery.webhook_deliveries (plugin, hook, received_at DESC);
CREATE UNIQUE INDEX webhook_deliveries_replay ON rookery.webhook_deliveries (plugin, hook, replay_key)
    WHERE replay_key IS NOT NULL AND outcome = 'accepted';
