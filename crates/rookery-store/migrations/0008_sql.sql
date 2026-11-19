-- Raw SQL of plugins. One row per plugin that has it: its role, whether its services may connect
-- directly, and the role's password, sealed like a secret.
CREATE TABLE rookery.plugin_sql (
    plugin           text PRIMARY KEY,
    role_name        text NOT NULL,
    direct           boolean NOT NULL DEFAULT false,
    key_id           text NOT NULL,
    password_nonce   bytea NOT NULL,
    password_sealed  bytea NOT NULL,
    -- How often the credential was replaced, so that a pool opened with an older one opens again.
    password_version integer NOT NULL DEFAULT 1,
    -- When the watchdog first ended a session of the role that the server did not open, on a plugin
    -- without direct connections: once per credential, so that a plugin that keeps trying does not
    -- fill the audit log.
    direct_bypass_reported_at timestamptz,
    updated_at       timestamptz NOT NULL DEFAULT now()
);

-- The SQL migrations (migrations/sql/<version>_<name>.sql) a plugin's raw schema has had.
CREATE TABLE rookery.plugin_sql_migrations (
    plugin     text NOT NULL,
    version    bigint NOT NULL,
    name       text NOT NULL,
    checksum   text NOT NULL,
    applied_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plugin, version)
);

-- The sessions the server itself opened as a plugin's role, named by their backend's pid and start time,
-- which a session cannot forge and no other reuses. Only the server writes it: no plugin role has any
-- privilege on the schema rookery. The watchdog ends sessions of a role that are not here, and deletes
-- the rows of sessions that are gone.
--   pool:      a pooled connection that runs a plugin's sql.* statements, under the limits;
--   migration: the connection that runs a plugin's migrations/sql/ files, with a longer statement time.
CREATE TABLE rookery.plugin_sql_sessions (
    pid           integer NOT NULL,
    backend_start timestamptz NOT NULL,
    role_name     text NOT NULL,
    purpose       text NOT NULL CHECK (purpose IN ('pool', 'migration')),
    opened_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (pid, backend_start)
);
