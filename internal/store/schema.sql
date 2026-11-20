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
-- ---------------------------------------------------------------- data
--
-- The datastore's shared tables: the change feed, references between records,
-- and the entities search reads. A plugin's collections live in its own schema,
-- p_<plugin>, and each collection's history in its own table beside it; the
-- core keeps only what it has to read across them.

-- Entity search's embeddings are pgvector columns. A database cloned from the
-- storetest template carries the extension with it.
CREATE EXTENSION IF NOT EXISTS vector;

-- changes is the datastore's change feed: one row per write, in one global
-- order. The writer inserts its row in the transaction that writes the record,
-- and notifies rookery_changes after it commits, so the feed reads a change only
-- once the record is there.
CREATE TABLE rookery.changes (
	seq        bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
	collection text        NOT NULL,
	resource   text        NOT NULL,
	kind       text        NOT NULL,
	rev        bigint      NOT NULL,
	space_id   text,
	owner_id   text,
	principal  text        NOT NULL,
	data       bytea,
	at         timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.changes IS 'One row per datastore write, in one order: the live queries and the change feed read them.';
COMMENT ON COLUMN rookery.changes.kind IS 'insert, update, delete or undelete.';
COMMENT ON COLUMN rookery.changes.data IS 'The record as a CBOR map, or null for a delete.';

CREATE INDEX changes_collection_seq ON rookery.changes (collection, seq);
CREATE INDEX changes_resource_seq ON rookery.changes (resource, seq);

-- refs records every reference a record holds: the record that holds it, the
-- field, the target URI, and what deleting the target does to the holder.
CREATE TABLE rookery.refs (
	source    text NOT NULL,
	field     text NOT NULL,
	target    text NOT NULL,
	on_delete text NOT NULL,
	PRIMARY KEY (source, field, target)
);

COMMENT ON TABLE rookery.refs IS 'The references records hold, for RefsTo, RefsFrom and what a delete does to a holder.';
COMMENT ON COLUMN rookery.refs.source IS 'The holding record, as a record:// URI.';
COMMENT ON COLUMN rookery.refs.on_delete IS 'restrict, nullify or cascade.';

CREATE INDEX refs_target ON rookery.refs (target);

-- entities holds what search and resolution need of every record whose
-- collection declares an entity block, and of every document that is one. The
-- core computes no embedding: a plugin that has one writes it.
CREATE TABLE rookery.entities (
	uri        text        PRIMARY KEY,
	kind       text        NOT NULL,
	title      text        NOT NULL,
	body       text        NOT NULL,
	search     tsvector    GENERATED ALWAYS AS (to_tsvector('simple', title || ' ' || body)) STORED,
	embedding  vector,
	access     text        NOT NULL,
	updated_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.entities IS 'What search resolves: a record or document by URI, with its title, its searchable text and its access parent.';
COMMENT ON COLUMN rookery.entities.access IS 'The resource URI whose grants decide who may see the entity.';

CREATE INDEX entities_kind ON rookery.entities (kind);
CREATE INDEX entities_search ON rookery.entities USING gin (search);
CREATE INDEX entities_access ON rookery.entities (access);
