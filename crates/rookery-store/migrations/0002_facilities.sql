-- The framework's facilities: access, settings, secrets, key-value storage, connections, blobs,
-- layouts, jobs and their log, leases, durable topics and notifications.

-- Groups of people. A group with a claim takes its members from the identity provider's groups.
CREATE TABLE rookery.groups (
    id         uuid PRIMARY KEY,
    name       text NOT NULL,
    claim      text UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (name)
);

CREATE TABLE rookery.group_members (
    group_id uuid NOT NULL REFERENCES rookery.groups (id) ON DELETE CASCADE,
    user_id  uuid NOT NULL REFERENCES rookery.users (id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, user_id)
);

CREATE INDEX group_members_user ON rookery.group_members (user_id);

-- Grants on any resource URI, such as env://studio. Levels: 1 view, 2 use, 3 manage. Principals:
-- user:<id>, group:<id>, everyone.
CREATE TABLE rookery.grants (
    resource   text NOT NULL,
    principal  text NOT NULL,
    level      smallint NOT NULL CHECK (level BETWEEN 1 AND 3),
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (resource, principal)
);

CREATE INDEX grants_principal ON rookery.grants (principal);

-- The owner of a resource holds manage on it.
CREATE TABLE rookery.owners (
    resource text PRIMARY KEY,
    user_id  uuid NOT NULL REFERENCES rookery.users (id) ON DELETE CASCADE
);

-- Settings of a plugin: a default the plugin sets, a global value an admin sets, a value of a user.
CREATE TABLE rookery.settings (
    plugin     text NOT NULL,
    key        text NOT NULL,
    scope      text NOT NULL CHECK (scope IN ('default', 'global', 'user')),
    user_id    uuid REFERENCES rookery.users (id) ON DELETE CASCADE,
    value      jsonb NOT NULL,
    updated_by text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((scope = 'user') = (user_id IS NOT NULL)),
    UNIQUE NULLS NOT DISTINCT (plugin, key, scope, user_id)
);

-- Sealed secrets: XChaCha20-Poly1305 under the key named by key_id, with associated data naming the row.
CREATE TABLE rookery.secrets (
    id         uuid PRIMARY KEY,
    plugin     text NOT NULL,
    name       text NOT NULL,
    scope      text NOT NULL CHECK (scope IN ('global', 'user')),
    user_id    uuid REFERENCES rookery.users (id) ON DELETE CASCADE,
    key_id     text NOT NULL,
    nonce      bytea NOT NULL,
    ciphertext bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((scope = 'user') = (user_id IS NOT NULL)),
    UNIQUE NULLS NOT DISTINCT (plugin, name, scope, user_id)
);

-- Key-value storage of plugins, with a revision for conditional writes.
CREATE TABLE rookery.kv (
    plugin     text NOT NULL,
    scope      text NOT NULL CHECK (scope IN ('global', 'user')),
    user_id    uuid REFERENCES rookery.users (id) ON DELETE CASCADE,
    key        text NOT NULL,
    value      jsonb NOT NULL,
    revision   bigint NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((scope = 'user') = (user_id IS NOT NULL)),
    UNIQUE NULLS NOT DISTINCT (plugin, scope, user_id, key)
);

-- Credentials of outside services. The config is plain, the credential sealed like a secret.
CREATE TABLE rookery.connections (
    id         uuid PRIMARY KEY,
    type       text NOT NULL,
    name       text NOT NULL,
    scope      text NOT NULL CHECK (scope IN ('global', 'user')),
    user_id    uuid REFERENCES rookery.users (id) ON DELETE CASCADE,
    config     jsonb NOT NULL DEFAULT '{}',
    key_id     text,
    nonce      bytea,
    ciphertext bytea,
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((scope = 'user') = (user_id IS NOT NULL)),
    UNIQUE NULLS NOT DISTINCT (type, name, scope, user_id)
);

-- Blobs: the bytes are files under ROOKERY_BLOBS, named by their SHA-256.
CREATE TABLE rookery.blobs (
    id           uuid PRIMARY KEY,
    sha256       text NOT NULL,
    size         bigint NOT NULL,
    content_type text NOT NULL,
    name         text,
    -- Who may read it: the principal that put it, besides admins.
    owner        text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX blobs_sha256 ON rookery.blobs (sha256);

-- A person's layout of a shell, by name.
CREATE TABLE rookery.layouts (
    user_id    uuid NOT NULL REFERENCES rookery.users (id) ON DELETE CASCADE,
    name       text NOT NULL,
    value      jsonb NOT NULL,
    revision   bigint NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, name)
);

-- Jobs. A handler inside the server claims one; cancel_requested asks it to stop.
CREATE TABLE rookery.jobs (
    id               uuid PRIMARY KEY,
    kind             text NOT NULL,
    params           jsonb NOT NULL DEFAULT '{}',
    state            text NOT NULL CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    attempts         integer NOT NULL DEFAULT 0,
    max_attempts     integer NOT NULL DEFAULT 1 CHECK (max_attempts >= 1),
    run_after        timestamptz NOT NULL DEFAULT now(),
    claimed_by       text,
    claimed_until    timestamptz,
    cancel_requested boolean NOT NULL DEFAULT false,
    progress         jsonb,
    result           jsonb,
    error            jsonb,
    -- A resource the job is about, such as env://studio.
    subject          text,
    enqueued_by      text NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    started_at       timestamptz,
    finished_at      timestamptz
);

CREATE INDEX jobs_ready ON rookery.jobs (run_after) WHERE state = 'queued';
CREATE INDEX jobs_created ON rookery.jobs (created_at DESC);
CREATE INDEX jobs_subject ON rookery.jobs (subject, created_at DESC);

-- A job's output, in order.
CREATE TABLE rookery.log_items (
    job_id  uuid NOT NULL REFERENCES rookery.jobs (id) ON DELETE CASCADE,
    seq     bigint NOT NULL,
    at      timestamptz NOT NULL DEFAULT now(),
    attempt integer NOT NULL,
    level   text NOT NULL CHECK (level IN ('debug', 'info', 'warn', 'error')),
    text    text NOT NULL,
    data    jsonb,
    PRIMARY KEY (job_id, seq)
);

-- Leases with fencing: the fence grows each time the lease changes hands, and stays when it is released.
CREATE TABLE rookery.leases (
    name       text PRIMARY KEY,
    holder     text,
    fence      bigint NOT NULL,
    expires_at timestamptz
);

-- Durable topics: messages in order, a cursor per subscription, dead letters.
CREATE TABLE rookery.topic_messages (
    seq          bigserial PRIMARY KEY,
    topic        text NOT NULL,
    payload      jsonb NOT NULL,
    published_by text NOT NULL,
    published_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX topic_messages_topic ON rookery.topic_messages (topic, seq);

CREATE TABLE rookery.topic_subscriptions (
    topic        text NOT NULL,
    subscription text NOT NULL,
    -- Every message up to here is acknowledged or dead.
    acked        bigint NOT NULL,
    -- Deliveries of the message after acked, and when it may be delivered again.
    attempts     integer NOT NULL DEFAULT 0,
    not_before   timestamptz,
    last_error   text,
    PRIMARY KEY (topic, subscription)
);

CREATE TABLE rookery.dead_letters (
    topic        text NOT NULL,
    subscription text NOT NULL,
    seq          bigint NOT NULL REFERENCES rookery.topic_messages (seq) ON DELETE CASCADE,
    attempts     integer NOT NULL,
    error        text,
    -- Delivered to its subscription again, before the messages after its cursor.
    retry        boolean NOT NULL DEFAULT false,
    at           timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (topic, subscription, seq)
);

-- Notifications, one inbox per person.
CREATE TABLE rookery.notifications (
    id          uuid PRIMARY KEY,
    user_id     uuid NOT NULL REFERENCES rookery.users (id) ON DELETE CASCADE,
    kind        text NOT NULL,
    title       text NOT NULL,
    body        text,
    data        jsonb,
    -- The answers it offers, such as ["approve", "deny"], and the one given.
    actions     jsonb,
    answer      text,
    sent_by     text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    read_at     timestamptz,
    answered_at timestamptz
);

CREATE INDEX notifications_user ON rookery.notifications (user_id, created_at DESC);

CREATE TABLE rookery.notification_prefs (
    user_id uuid NOT NULL REFERENCES rookery.users (id) ON DELETE CASCADE,
    kind    text NOT NULL,
    muted   boolean NOT NULL,
    PRIMARY KEY (user_id, kind)
);
