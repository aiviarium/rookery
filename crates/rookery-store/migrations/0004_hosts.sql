-- Hosts and what Rookery keeps of them: profiles, SSH access, the key ring, provisioners, the machines
-- they made, and the messages taken in from the hosts' channels. Ids are the 32 hex digits of a UUID
-- of version 7, as Rookery has always written them, or a name of the caller's for a resource.

-- A saved host. env://NAME names a row; its owner holds manage on it.
CREATE TABLE rookery.hosts (
    id         text PRIMARY KEY,
    name       text NOT NULL UNIQUE,
    profile    jsonb NOT NULL,
    -- A profile from before types is a workstation.
    type       text GENERATED ALWAYS AS (coalesce(profile->>'type', 'workstation')) STORED,
    owner      uuid REFERENCES rookery.users (id) ON DELETE SET NULL,
    -- What the leader last saw of it: online, offline or unknown.
    state      text NOT NULL DEFAULT 'unknown',
    seen_at    timestamptz,
    -- Its device on the tailnet, and a new name it is to take.
    tailnet    jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- The host keys a host was seen with, and the keys it trusts. A host's first contact, as when it is
-- adopted, records them before its profile is saved, so no key ties them to rookery.hosts: they go
-- with their host by the trigger below.
CREATE TABLE rookery.host_access (
    host       text PRIMARY KEY,
    value      jsonb NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- The key ring: private keys Rookery logs in to hosts with, sealed like a secret.
CREATE TABLE rookery.host_keys (
    id         text PRIMARY KEY,
    name       text UNIQUE,
    key_id     text NOT NULL,
    nonce      bytea NOT NULL,
    ciphertext bytea NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Provisioners: an entry is a provisioner under a name of its own, with its type and settings; the
-- arguments of a plugin's provisioner are kept by PLUGIN/ID.
CREATE TABLE rookery.provisioners (
    id         text PRIMARY KEY,
    name       text UNIQUE,
    kind       text NOT NULL CHECK (kind IN ('entry', 'arguments')),
    value      jsonb NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((kind = 'entry') = (name IS NOT NULL))
);

-- What provisioners made: a resource's record and the specification it was made from.
CREATE TABLE rookery.resources (
    id         text PRIMARY KEY,
    record     jsonb,
    spec       jsonb,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (record IS NOT NULL OR spec IS NOT NULL)
);

-- Messages of the hosts' channels that are kept, by the id of the host, and how far each host's
-- channel was read.
CREATE TABLE rookery.host_messages (
    seq        bigserial PRIMARY KEY,
    host       text NOT NULL REFERENCES rookery.hosts (id) ON DELETE CASCADE ON UPDATE CASCADE DEFERRABLE INITIALLY DEFERRED,
    id         bigint NOT NULL,
    topic      text NOT NULL,
    body       jsonb NOT NULL,
    keep       text NOT NULL CHECK (keep IN ('last', 'log')),
    from_level text NOT NULL,
    from_uid   bigint NOT NULL,
    session    text,
    time       timestamptz NOT NULL,
    done       boolean NOT NULL DEFAULT false,
    UNIQUE (host, id)
);

-- What is kept of a host goes with it. The messages and the channel's cursor are a linked host's
-- alone, so they reference it, checked at commit so that a batch writes a host and its rows in any
-- order.
CREATE FUNCTION rookery.host_gone() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM rookery.host_access WHERE host = OLD.id;
    RETURN OLD;
END
$$;

CREATE TRIGGER hosts_gone AFTER DELETE ON rookery.hosts FOR EACH ROW EXECUTE FUNCTION rookery.host_gone();

CREATE INDEX host_messages_topic ON rookery.host_messages (host, topic);
CREATE INDEX host_messages_time ON rookery.host_messages (time);

CREATE TABLE rookery.host_cursors (
    host    text PRIMARY KEY REFERENCES rookery.hosts (id) ON DELETE CASCADE ON UPDATE CASCADE DEFERRABLE INITIALLY DEFERRED,
    last_id bigint NOT NULL
);

-- The state of the installed plugins: hatchery.kdl, eggs.lock and seen.toml, which the leader writes
-- to its plugin folder before it opens a link.
CREATE TABLE rookery.package_state (
    name       text PRIMARY KEY,
    value      bytea NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- A job of a host action: a token that finds the job a repeated request started, and the session on
-- the host an action runs as, so that a new leader follows it on. How far its output was copied is
-- in the log item that ends there (data.session_offset); session_offset is where it started. The
-- values its output masks are sealed in hidden, as a JSON list, for the leader that follows it on.
ALTER TABLE rookery.jobs
    ADD COLUMN token text,
    ADD COLUMN session text,
    ADD COLUMN session_offset bigint,
    ADD COLUMN hidden_key_id text,
    ADD COLUMN hidden_nonce bytea,
    ADD COLUMN hidden bytea;

CREATE UNIQUE INDEX jobs_token ON rookery.jobs (subject, kind, (params->>'action'), token) WHERE token IS NOT NULL;
