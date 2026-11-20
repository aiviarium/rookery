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

-- ---------------------------------------------------------------------------
-- Jobs
-- ---------------------------------------------------------------------------

CREATE TABLE rookery.jobs (
	id            text        PRIMARY KEY,
	kind          text        NOT NULL,
	args          bytea       NOT NULL DEFAULT ''::bytea,
	resource      text        NOT NULL DEFAULT '',
	created_by    text        NOT NULL DEFAULT '',
	parent        text        NOT NULL DEFAULT '',
	state         text        NOT NULL DEFAULT 'queued'
		CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'cancelled', 'interrupted')),
	attempts      integer     NOT NULL DEFAULT 0,
	max_attempts  integer     NOT NULL DEFAULT 1,
	-- host is the host a core kind is about: at most one job per host runs.
	host          text        NOT NULL DEFAULT '',
	-- claim is a plugin worker's token for the job it holds.
	claim         text,
	heartbeat_at  timestamptz,
	-- available_at defers a queued job, for a retry's backoff.
	available_at  timestamptz NOT NULL DEFAULT now(),
	result        bytea,
	error         text        NOT NULL DEFAULT '',
	-- cancel_requested is set when a running job is asked to stop.
	cancel_requested boolean  NOT NULL DEFAULT false,
	created_at    timestamptz NOT NULL DEFAULT now(),
	started_at    timestamptz,
	finished_at   timestamptz
);

COMMENT ON TABLE rookery.jobs IS 'Every job: its kind, arguments, resource, state and log.';

CREATE INDEX jobs_claimable ON rookery.jobs (state, available_at, id) WHERE state = 'queued';
CREATE INDEX jobs_running ON rookery.jobs (state, heartbeat_at) WHERE state = 'running';
CREATE INDEX jobs_resource ON rookery.jobs (resource, id);
CREATE INDEX jobs_finished ON rookery.jobs (finished_at) WHERE finished_at IS NOT NULL;

CREATE TABLE rookery.job_lines (
	id     bigserial   PRIMARY KEY,
	job_id text        NOT NULL REFERENCES rookery.jobs (id) ON DELETE CASCADE,
	at     timestamptz NOT NULL DEFAULT now(),
	line   text        NOT NULL
);

COMMENT ON TABLE rookery.job_lines IS 'A job''s log, a line at a time, in the order it was written.';

CREATE INDEX job_lines_job ON rookery.job_lines (job_id, id);

CREATE TABLE rookery.schedules (
	id          text        PRIMARY KEY,
	name        text        NOT NULL UNIQUE,
	kind        text        NOT NULL,
	args        bytea       NOT NULL DEFAULT ''::bytea,
	resource    text        NOT NULL DEFAULT '',
	cron        text        NOT NULL,
	timezone    text        NOT NULL DEFAULT 'UTC',
	enabled     boolean     NOT NULL DEFAULT true,
	next_run_at timestamptz,
	last_run_at timestamptz,
	created_at  timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.schedules IS 'Jobs the deployment runs on a cron expression.';

CREATE INDEX schedules_due ON rookery.schedules (next_run_at) WHERE enabled;

-- ---------------------------------------------------------------------------
-- Messaging
-- ---------------------------------------------------------------------------

CREATE TABLE rookery.topics (
	name       text        PRIMARY KEY,
	created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE rookery.topic_messages (
	id           bigserial   PRIMARY KEY,
	topic        text        NOT NULL,
	payload      bytea       NOT NULL,
	attempts     integer     NOT NULL DEFAULT 0,
	available_at timestamptz NOT NULL DEFAULT now(),
	created_at   timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.topic_messages IS 'Durable topic messages, in the order they were published.';

CREATE INDEX topic_messages_topic ON rookery.topic_messages (topic, id);

CREATE TABLE rookery.topic_cursors (
	topic      text        NOT NULL,
	consumer   text        NOT NULL,
	next_seq   bigint      NOT NULL DEFAULT 0,
	updated_at timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (topic, consumer)
);

COMMENT ON TABLE rookery.topic_cursors IS 'Where each consumer group of a topic has got to.';

CREATE TABLE rookery.topic_dead (
	id       bigserial   PRIMARY KEY,
	topic    text        NOT NULL,
	consumer text        NOT NULL,
	payload  bytea       NOT NULL,
	attempts integer     NOT NULL,
	error    text        NOT NULL DEFAULT '',
	at       timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.topic_dead IS 'Messages a consumer gave up on, for an admin to retry or discard.';

CREATE TABLE rookery.leases (
	name       text        PRIMARY KEY,
	holder     text        NOT NULL,
	fencing    bigint      NOT NULL,
	expires_at timestamptz NOT NULL,
	updated_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.leases IS 'Named locks whose fencing number rises on each acquire.';

CREATE TABLE rookery.notifications (
	id         text        PRIMARY KEY,
	user_id    text        NOT NULL REFERENCES rookery.users (id) ON DELETE CASCADE,
	title      text        NOT NULL,
	body       text        NOT NULL DEFAULT '',
	link       text        NOT NULL DEFAULT '',
	resource   text        NOT NULL DEFAULT '',
	actions    bytea,
	answers    bytea,
	read_at    timestamptz,
	created_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.notifications IS 'What a user is told about, and what they answered.';

CREATE INDEX notifications_user ON rookery.notifications (user_id, id);

CREATE TABLE rookery.notification_prefs (
	user_id    text        PRIMARY KEY REFERENCES rookery.users (id) ON DELETE CASCADE,
	prefs      bytea       NOT NULL,
	updated_at timestamptz NOT NULL DEFAULT now()
);



-- ---------------------------------------------------------------------------
-- Provisioners
-- ---------------------------------------------------------------------------
--
-- An entry is a provisioner type an installed plugin declares, with arguments,
-- and either the managed host whose engine it drives (a node) or the address of
-- an engine it reaches itself. A resource is something an entry made: the core
-- records it, lists it and removes it, and knows one kind itself, machine, of
-- which a host has at most one.

CREATE TABLE rookery.provisioners (
	name       text        PRIMARY KEY,
	-- type is the provisioner type's name, "plugin/id", such as
	-- "docker/container".
	type       text        NOT NULL CHECK (type LIKE '%/%'),
	plugin     text        NOT NULL,
	args       bytea       NOT NULL,
	-- node is the managed host whose engine the entry drives. It and remote are
	-- both empty for the server's own engine.
	node       text        NOT NULL DEFAULT '',
	remote     text        NOT NULL DEFAULT '',
	created_at timestamptz NOT NULL DEFAULT now(),
	updated_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.provisioners IS 'Provisioner entries: a type, its CBOR arguments, and where its engine is.';

-- provisioned_resources is what provisioner entries made. It is not
-- rookery.resources, which holds the URIs grants name, although each resource
-- row here also has a URI in that table's terms once the datastore needs it.
CREATE TABLE rookery.provisioned_resources (
	id           text        PRIMARY KEY,
	kind         text        NOT NULL,
	-- name is what the entry called it, and for a machine the host's name.
	name         text        NOT NULL,
	provisioner  text        NOT NULL REFERENCES rookery.provisioners (name) ON DELETE RESTRICT,
	state        bytea       NOT NULL,
	address      text        NOT NULL DEFAULT '',
	created_at   timestamptz NOT NULL DEFAULT now(),
	updated_at   timestamptz NOT NULL DEFAULT now(),
	UNIQUE (provisioner, kind, name)
);

COMMENT ON TABLE rookery.provisioned_resources IS 'Machines and other resources provisioner entries made, with the CBOR state that reaches them.';

-- A host has at most one machine.
CREATE UNIQUE INDEX provisioned_resources_one_machine ON rookery.provisioned_resources (name) WHERE kind = 'machine';
CREATE INDEX provisioned_resources_by_provisioner ON rookery.provisioned_resources (provisioner, id);
<<<<<<< HEAD
-- ---------------------------------------------------------------------------
-- Plugins. A row per installed package: its manifest, its hash, whether it is
-- enabled, and where its files are (ROOKERY_STATE_DIR/plugins/<id>/<sha256>/).
-- ---------------------------------------------------------------------------

CREATE TABLE rookery.plugins (
	id           text        PRIMARY KEY,
	name         text        NOT NULL DEFAULT '',
	version      text        NOT NULL,
	sha256       text        NOT NULL,
	manifest     bytea       NOT NULL,
	enabled      boolean     NOT NULL DEFAULT true,
	installed_at timestamptz NOT NULL DEFAULT now(),
	updated_at   timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.plugins IS 'The packages installed in this deployment, their manifests and their hashes.';
COMMENT ON COLUMN rookery.plugins.manifest IS 'The plugin.yaml the package was installed with.';
COMMENT ON COLUMN rookery.plugins.sha256 IS 'The package hash, which is also the folder its files are in.';

CREATE INDEX plugins_enabled ON rookery.plugins (enabled, id);
=======


-- ---------------------------------------------------------------------------
-- Settings, secrets, connections and blobs
-- ---------------------------------------------------------------------------

CREATE TABLE rookery.settings (
	key        text        NOT NULL,
	-- scope is global, space or user.
	scope      text        NOT NULL CHECK (scope IN ('global', 'space', 'user')),
	-- scope_id is '' for global, the space's id, or the user's id.
	scope_id   text        NOT NULL DEFAULT '',
	value      bytea       NOT NULL,
	updated_at timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (key, scope, scope_id)
);

COMMENT ON TABLE rookery.settings IS 'Setting values per scope; the definition and its default are the server''s.';

CREATE TABLE rookery.secrets (
	name       text        NOT NULL,
	-- scope is global, plugin/<id>, provisioner/<name> or host/<name>.
	scope      text        NOT NULL,
	value      bytea       NOT NULL,
	key_id     text        NOT NULL,
	created_at timestamptz NOT NULL DEFAULT now(),
	updated_at timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (name, scope)
);

COMMENT ON TABLE rookery.secrets IS 'Sealed secrets, by name and scope, with the key that sealed each one.';

CREATE TABLE rookery.connections (
	id         text        PRIMARY KEY,
	type       text        NOT NULL,
	name       text        NOT NULL UNIQUE,
	values     bytea       NOT NULL,
	key_id     text        NOT NULL,
	created_at timestamptz NOT NULL DEFAULT now(),
	updated_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.connections IS 'External credentials of a declared type, sealed as one value.';

CREATE TABLE rookery.blobs (
	id           text        PRIMARY KEY,
	size         bigint      NOT NULL,
	content_type text        NOT NULL DEFAULT '',
	owner        text        NOT NULL DEFAULT '',
	resource     text        NOT NULL DEFAULT '',
	pinned       boolean     NOT NULL DEFAULT false,
	created_at   timestamptz NOT NULL DEFAULT now(),
	referenced_at timestamptz
);

COMMENT ON TABLE rookery.blobs IS 'Blob metadata: size, type, owner and the resource the blob belongs to.';
>>>>>>> 4e7366a (Add settings, secrets, connections and blobs: scopes, sealing, rekey and the two backends)
