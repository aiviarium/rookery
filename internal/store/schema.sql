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

-- The datastore catalog stores the declarations that created the tables.
CREATE TABLE rookery.collections (
	plugin      text  NOT NULL,
	name        text  NOT NULL,
	declaration bytea NOT NULL,
	PRIMARY KEY (plugin, name)
);

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
	blob_id text GENERATED ALWAYS AS (
		CASE WHEN starts_with(target, 'blob://') THEN substr(target, 8) END
	) STORED,
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

-- sql_roles is the Postgres role of each plugin that declares `database: sql`.
-- The role owns p_<plugin>_sql and may read the plugin's own collections; its
-- password is sealed under the deployment's key, with the plugin's id as the
-- associated data.
CREATE TABLE rookery.sql_roles (
	plugin     text        PRIMARY KEY,
	role       text        NOT NULL,
	password   bytea       NOT NULL,
	key_id     text        NOT NULL,
	created_at timestamptz NOT NULL DEFAULT now(),
	updated_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.sql_roles IS 'The raw SQL role of each plugin that declares database: sql, with its sealed password.';

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
ALTER TABLE rookery.refs ADD FOREIGN KEY (blob_id) REFERENCES rookery.blobs (id);
CREATE INDEX refs_blob_id ON rookery.refs (blob_id) WHERE blob_id IS NOT NULL;

-- A reference protects its blob immediately and starts the grace period
-- afresh when it is removed.
CREATE FUNCTION rookery.touch_blob_reference() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
	IF TG_OP = 'DELETE' THEN
		UPDATE rookery.blobs SET referenced_at = now() WHERE id = OLD.blob_id;
	ELSE
		UPDATE rookery.blobs SET referenced_at = now() WHERE id = NEW.blob_id;
	END IF;
	RETURN NULL;
END;
$$;

CREATE TRIGGER touch_blob_reference
AFTER INSERT OR DELETE ON rookery.refs
FOR EACH ROW EXECUTE FUNCTION rookery.touch_blob_reference();

-- Live documents. A document is a type of CRDT document and a parent resource
-- its access comes from; its updates are an append-only log, and a snapshot is
-- a compacted state with the sequence it covers. The core never reads an
-- update: a Yjs or another CRDT client makes and consumes them.

CREATE TABLE rookery.documents (
	id         text        PRIMARY KEY,
	type       text        NOT NULL,
	parent     text        NOT NULL,
	owner_id   text        NOT NULL,
	head       bigint      NOT NULL DEFAULT 0,
	created_at timestamptz NOT NULL DEFAULT now(),
	updated_at timestamptz NOT NULL DEFAULT now(),
	deleted_at timestamptz
);

COMMENT ON TABLE rookery.documents IS 'Live documents: a CRDT type and a parent resource whose grants decide access.';
COMMENT ON COLUMN rookery.documents.type IS 'The CRDT a client opens it with, such as yjs.';
COMMENT ON COLUMN rookery.documents.parent IS 'The resource URI the document hangs under, as document://<id> is its own resource.';

CREATE INDEX documents_parent ON rookery.documents (parent);

CREATE TABLE rookery.document_updates (
	document_id text        NOT NULL,
	seq         bigint      NOT NULL,
	data        bytea       NOT NULL,
	created_at  timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (document_id, seq)
);

COMMENT ON TABLE rookery.document_updates IS 'The append-only log of updates to documents, in the order they were appended.';
COMMENT ON COLUMN rookery.document_updates.data IS 'The CRDT update, which the core relays without reading.';

CREATE INDEX document_updates_created ON rookery.document_updates (created_at);

CREATE TABLE rookery.document_snapshots (
	document_id text        NOT NULL,
	seq         bigint      NOT NULL,
	data        bytea       NOT NULL,
	version     boolean     NOT NULL DEFAULT false,
	label       text,
	created_by  text        NOT NULL,
	created_at  timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (document_id, seq)
);

COMMENT ON TABLE rookery.document_snapshots IS 'Compacted document states, each with the sequence it covers; the ones marked as versions are the document history.';
COMMENT ON COLUMN rookery.document_snapshots.seq IS 'The last update the snapshot covers; the updates up to it go a day after it was written.';

CREATE INDEX document_snapshots_versions ON rookery.document_snapshots (document_id, seq) WHERE version;
-- --------------------------------------------------- hosts, links and keys
-- ── internal/hosts, internal/link ────────────────────────────────────────────
-- The machines Rookery runs: their profile, the type they have now and the
-- digest of what an apply last put on them; their labels; the SSH keys the
-- server logs in with; the host keys it trusts; and where each host's channel
-- follower stands.
--
-- The keys and known hosts are `internal/link`'s: the ring's private keys are
-- sealed with the deployment's key, and a host key is trusted on first use and
-- refused when it changes until an admin trusts the new one.

CREATE TABLE rookery.hosts (
	name           text        PRIMARY KEY,
	-- type is what the host can do now; the profile's type is what it should be.
	type           text        NOT NULL CHECK (type IN ('ssh', 'managed', 'workstation')),
	-- profile is the HostProfile as CBOR in Core Deterministic mode.
	profile        bytea       NOT NULL,
	-- address is the host's tailnet address once it has joined one, empty before.
	address        text        NOT NULL DEFAULT '',
	applied_digest text        NOT NULL DEFAULT '',
	applied_at     timestamptz,
	-- last_seen_at is when a link to the host last answered.
	last_seen_at   timestamptz,
	created_by     text        REFERENCES rookery.users (id) ON DELETE SET NULL,
	created_at     timestamptz NOT NULL DEFAULT now(),
	updated_at     timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.hosts IS 'The machines Rookery runs, by name, with the profile an apply makes them and what they are now.';
COMMENT ON COLUMN rookery.hosts.address IS 'The tailnet address a host that joined one is reached at; empty for one reached at its profile address.';
COMMENT ON COLUMN rookery.hosts.applied_digest IS 'The digest of the profile an apply last put on the host; a difference from the current digest means it is behind.';

CREATE TABLE rookery.host_labels (
	host  text NOT NULL REFERENCES rookery.hosts (name) ON UPDATE CASCADE ON DELETE CASCADE,
	key   text NOT NULL,
	value text NOT NULL,
	PRIMARY KEY (host, key)
);

COMMENT ON TABLE rookery.host_labels IS 'Labels on a host, for roll-outs and filters: role=desk, site=lab.';

CREATE INDEX host_labels_key ON rookery.host_labels (key, value);

-- keys is the ring: the SSH keys the server logs in to its hosts with. One key
-- is active; a rotation retires the old one, which is kept until no host's
-- authorized_keys names it any more.
CREATE TABLE rookery.keys (
	id          text        PRIMARY KEY,
	name        text        NOT NULL,
	-- key_type is the SSH key algorithm, such as ssh-ed25519.
	key_type    text        NOT NULL,
	public_key  bytea       NOT NULL,
	-- private_key is sealed with the deployment's key, the row's id as
	-- associated data, and key_id names the sealing key.
	private_key bytea       NOT NULL,
	key_id      text        NOT NULL,
	fingerprint text        NOT NULL,
	created_at  timestamptz NOT NULL DEFAULT now(),
	retired_at  timestamptz
);

COMMENT ON TABLE rookery.keys IS 'The SSH key ring: the keys the server logs in with, their private halves sealed.';
COMMENT ON COLUMN rookery.keys.key_id IS 'The id of the sealing key the private half is sealed with, for a rekey.';

CREATE UNIQUE INDEX keys_one_active ON rookery.keys ((true)) WHERE retired_at IS NULL;

-- known_hosts is the host keys the server trusts, by the address it dials as
-- knownhosts writes one: "192.0.2.4:22" or "[2001:db8::1]:22". First contact
-- records the key it was given; a later connection that presents another one is
-- refused, and the offered key waits in pending_* until an admin trusts it.
CREATE TABLE rookery.known_hosts (
	address             text        PRIMARY KEY,
	host                text        NOT NULL DEFAULT '',
	key_type            text        NOT NULL,
	public_key          bytea       NOT NULL,
	fingerprint         text        NOT NULL,
	pending_key_type    text,
	pending_public_key  bytea,
	pending_fingerprint text,
	pending_at          timestamptz,
	first_seen_at       timestamptz NOT NULL DEFAULT now(),
	trusted_at          timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.known_hosts IS 'The host keys the server trusts, by address, with the key a mismatch offered for an admin to trust.';

-- host_cursors is where each managed host's channel follower stands: the
-- sequence of the last message taken in, and published to the server's topics.
CREATE TABLE rookery.host_cursors (
	host       text        PRIMARY KEY REFERENCES rookery.hosts (name) ON UPDATE CASCADE ON DELETE CASCADE,
	sequence   bigint      NOT NULL DEFAULT 0,
	updated_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE rookery.host_cursors IS 'The last channel message taken in from each host, for the follower to continue from.';
-- Roll-outs. A roll-out applies, or runs one action, on a set of hosts in
-- batches. It keeps a row per host, and each host's work is a child job whose
-- parent is the roll-out.
-- ---------------------------------------------------------------------------

CREATE TABLE rookery.rollouts (
	id             text        PRIMARY KEY,
	kind           text        NOT NULL,
	args           bytea       NOT NULL DEFAULT ''::bytea,
	resource       text        NOT NULL DEFAULT '',
	created_by     text        NOT NULL DEFAULT '',
	state          text        NOT NULL DEFAULT 'running'
		CHECK (state IN ('running', 'paused', 'succeeded', 'failed', 'cancelled')),
	selection      bytea       NOT NULL DEFAULT ''::bytea,
	strategy       bytea       NOT NULL DEFAULT ''::bytea,
	canary_done    boolean     NOT NULL DEFAULT false,
	paused_until   timestamptz,
	paused_because text        NOT NULL DEFAULT '',
	created_at     timestamptz NOT NULL DEFAULT now(),
	started_at     timestamptz,
	finished_at    timestamptz
);

COMMENT ON TABLE rookery.rollouts IS 'An apply or one action on a set of hosts, in batches.';
COMMENT ON COLUMN rookery.rollouts.selection IS 'The selection as CBOR, so a roll-out says what it was for even after its hosts changed.';
COMMENT ON COLUMN rookery.rollouts.strategy IS 'The strategy as CBOR: batch size, failures allowed, the pause and the canary.';
COMMENT ON COLUMN rookery.rollouts.paused_until IS 'When the pause between batches ends; set while the roll-out is still running.';

CREATE INDEX rollouts_state ON rookery.rollouts (state, id);

CREATE TABLE rookery.rollout_hosts (
	rollout_id  text        NOT NULL REFERENCES rookery.rollouts (id) ON DELETE CASCADE,
	host        text        NOT NULL,
	batch       integer     NOT NULL DEFAULT 0,
	state       text        NOT NULL DEFAULT 'pending'
		CHECK (state IN ('pending', 'running', 'succeeded', 'failed', 'skipped', 'cancelled')),
	job         text        NOT NULL DEFAULT '',
	reason      text        NOT NULL DEFAULT '',
	error       text        NOT NULL DEFAULT '',
	started_at  timestamptz,
	finished_at timestamptz,
	PRIMARY KEY (rollout_id, host)
);

COMMENT ON TABLE rookery.rollout_hosts IS 'What a roll-out does to one host: its batch, its child job and how it ended.';

CREATE INDEX rollout_hosts_state ON rookery.rollout_hosts (rollout_id, state, batch);

-- A schedule runs a job kind, or starts a roll-out when its rollout is set.
-- The column lives in this section to keep one item's changes in one place.
ALTER TABLE rookery.schedules ADD COLUMN rollout bytea NOT NULL DEFAULT ''::bytea;

COMMENT ON COLUMN rookery.schedules.rollout IS 'The roll-out to start as CBOR, instead of a job kind, when it is set.';
