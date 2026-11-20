-- The core schema.
--
-- Until the first tagged release this file is the whole core schema: a change
-- edits it, and the server records its SHA-256 in rookery.meta. A database whose
-- digest differs is refused at start-up, and `rookery server reset` drops the
-- schemas and applies this file again. At the first tag this file becomes
-- migration 1 and numbered migrations start from there.

CREATE SCHEMA rookery;

COMMENT ON SCHEMA rookery IS 'The Rookery core: people, access, hosts, jobs, data.';

CREATE TABLE rookery.meta (
	key        text        PRIMARY KEY,
	value      bytea       NOT NULL,
	updated_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.meta IS 'Values per database: the schema digest today, more later.';

-- ---------------------------------------------------------------------------
-- People and access
-- ---------------------------------------------------------------------------

CREATE TABLE rookery.users (
	id         text        PRIMARY KEY,
	email      text        NOT NULL UNIQUE,
	name       text        NOT NULL DEFAULT '',
	admin      boolean     NOT NULL DEFAULT false,
	disabled   boolean     NOT NULL DEFAULT false,
	created_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.users IS 'People who can sign in. The email is stored lower-cased.';

CREATE TABLE rookery.links (
	token_hash bytea       PRIMARY KEY,
	email      text        NOT NULL,
	user_id    text        REFERENCES rookery.users (id) ON DELETE CASCADE,
	created_by text        REFERENCES rookery.users (id) ON DELETE SET NULL,
	expires_at timestamptz NOT NULL,
	used_at    timestamptz,
	created_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.links IS 'One-time sign-in links, 24 hours, held by their token hash.';

CREATE TABLE rookery.sessions (
	id          text        PRIMARY KEY,
	secret_hash bytea       NOT NULL UNIQUE,
	user_id     text        NOT NULL REFERENCES rookery.users (id) ON DELETE CASCADE,
	csrf        text        NOT NULL,
	user_agent  text        NOT NULL DEFAULT '',
	created_at  timestamptz NOT NULL DEFAULT now(),
	seen_at     timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.sessions IS 'Device sessions: a browser cookie, held by its secret hash, with its CSRF token.';

CREATE TABLE rookery.tokens (
	id           text        PRIMARY KEY,
	user_id      text        NOT NULL REFERENCES rookery.users (id) ON DELETE CASCADE,
	name         text        NOT NULL,
	token_hash   bytea       NOT NULL UNIQUE,
	created_at   timestamptz NOT NULL DEFAULT now(),
	last_used_at timestamptz
);

COMMENT ON TABLE rookery.tokens IS 'Access tokens, rkpat_ ones, held by their SHA-256.';

CREATE TABLE rookery.groups (
	id         text        PRIMARY KEY,
	name       text        NOT NULL UNIQUE,
	created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE rookery.group_members (
	group_id text NOT NULL REFERENCES rookery.groups (id) ON DELETE CASCADE,
	user_id  text NOT NULL REFERENCES rookery.users (id) ON DELETE CASCADE,
	-- source is 'manual' for a member added by hand, and 'claim' for one an
	-- OIDC claim put there at sign-in.
	source   text NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'claim')),
	PRIMARY KEY (group_id, user_id)
);

CREATE TABLE rookery.group_claims (
	group_id text NOT NULL REFERENCES rookery.groups (id) ON DELETE CASCADE,
	claim    text NOT NULL,
	value    text NOT NULL,
	PRIMARY KEY (group_id, claim, value)
);

COMMENT ON TABLE rookery.group_claims IS 'OIDC claim values whose holders join the group, refreshed at each sign-in.';

CREATE TABLE rookery.resources (
	uri        text        PRIMARY KEY,
	parent     text        REFERENCES rookery.resources (uri) ON DELETE SET NULL,
	created_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.resources IS 'Every resource a grant can name, as a URI, with its parent.';

CREATE TABLE rookery.grants (
	resource   text        NOT NULL REFERENCES rookery.resources (uri) ON DELETE CASCADE,
	principal  text        NOT NULL,
	level      smallint    NOT NULL,
	created_at timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (resource, principal)
);

COMMENT ON TABLE rookery.grants IS 'Levels held on a resource by user:ID, group:ID or everyone.';

CREATE INDEX grants_principal ON rookery.grants (principal);

CREATE TABLE rookery.spaces (
	id         text        PRIMARY KEY,
	kind       text        NOT NULL,
	name       text        NOT NULL,
	archived   boolean     NOT NULL DEFAULT false,
	created_by text        REFERENCES rookery.users (id) ON DELETE SET NULL,
	created_at timestamptz NOT NULL DEFAULT now(),
	updated_at timestamptz NOT NULL DEFAULT now(),
	UNIQUE (kind, name)
);

CREATE TABLE rookery.audit (
	id        bigserial   PRIMARY KEY,
	at        timestamptz NOT NULL DEFAULT now(),
	principal text        NOT NULL,
	procedure text        NOT NULL,
	resource  text        NOT NULL DEFAULT '',
	outcome   text        NOT NULL,
	detail    text        NOT NULL DEFAULT ''
);

COMMENT ON TABLE rookery.audit IS 'Every call that changes state, or tries to: who, what, on what, how it ended.';

CREATE INDEX audit_at ON rookery.audit (at DESC);
-- ── internal/tailnet ─────────────────────────────────────────────────────────
-- The server's own tailnet node keeps its state here: its keys, its preferences
-- and what the control server told it. tsnet asks for it through ipn.StateStore,
-- so a standby that takes the lead has the same identity as the server it
-- replaces. Values are opaque.
CREATE TABLE rookery.tailnet_state (
	key        text        PRIMARY KEY,
	value      bytea       NOT NULL,
	updated_at timestamptz NOT NULL DEFAULT now()
);
