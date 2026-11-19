-- People, their device sessions and access tokens, one-time sign-in links, the server's own small
-- state and the audit log. Tokens and codes are stored as SHA-256 only.

CREATE TABLE rookery.server_state (
    key   text PRIMARY KEY,
    value text NOT NULL
);

CREATE TABLE rookery.users (
    id           uuid PRIMARY KEY,
    -- The identity provider's issuer, or rookery-local:<deployment id> for a local account.
    issuer       text NOT NULL,
    subject      text NOT NULL,
    name         text NOT NULL,
    email        text,
    role         text NOT NULL DEFAULT 'member' CHECK (role IN ('admin', 'member')),
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz,
    disabled_at  timestamptz,
    UNIQUE (issuer, subject)
);

CREATE INDEX users_email ON rookery.users (lower(email));

CREATE TABLE rookery.device_sessions (
    id           uuid PRIMARY KEY,
    user_id      uuid NOT NULL REFERENCES rookery.users (id) ON DELETE CASCADE,
    token_hash   bytea NOT NULL UNIQUE,
    label        text,
    user_agent   text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    expires_at   timestamptz,
    revoked_at   timestamptz
);

CREATE INDEX device_sessions_user ON rookery.device_sessions (user_id);

CREATE TABLE rookery.access_tokens (
    id           uuid PRIMARY KEY,
    user_id      uuid NOT NULL REFERENCES rookery.users (id) ON DELETE CASCADE,
    name         text NOT NULL,
    token_hash   bytea NOT NULL UNIQUE,
    hint         text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    expires_at   timestamptz,
    revoked_at   timestamptz
);

CREATE INDEX access_tokens_user ON rookery.access_tokens (user_id);

CREATE TABLE rookery.sign_in_links (
    id         uuid PRIMARY KEY,
    code_hash  bytea NOT NULL UNIQUE,
    kind       text NOT NULL CHECK (kind IN ('setup', 'invite', 'device')),
    -- Whom a device link signs in.
    user_id    uuid REFERENCES rookery.users (id) ON DELETE CASCADE,
    -- The name and email of the person an invite makes.
    name       text,
    email      text,
    created_by uuid REFERENCES rookery.users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    used_at    timestamptz,
    revoked_at timestamptz
);

CREATE INDEX sign_in_links_open ON rookery.sign_in_links (expires_at)
    WHERE used_at IS NULL AND revoked_at IS NULL;

CREATE TABLE rookery.audit (
    id        bigserial PRIMARY KEY,
    at        timestamptz NOT NULL DEFAULT now(),
    -- user:<id>, local:socket, system:<part>, plugin:<id>.
    principal text NOT NULL,
    action    text NOT NULL,
    resource  text,
    details   jsonb NOT NULL DEFAULT '{}'
);

CREATE INDEX audit_at ON rookery.audit (at DESC);
CREATE INDEX audit_resource ON rookery.audit (resource, at DESC);
