-- The data layer: collections of plugins and of Rookery itself, with their entities, references,
-- change log and history, spaces, and the access check that walks entities to their parents.

-- Every entity: anything with a URI. Rows of entity collections are written here in the write's
-- transaction; other things, such as spaces and documents, are registered by the server.
CREATE TABLE rookery.entities (
    scheme     text NOT NULL,
    id         text NOT NULL,
    uri        text GENERATED ALWAYS AS (scheme || '://' || id) STORED,
    space_id   uuid,
    -- Another entity's URI. Grants are inherited along it unless this entity is private.
    parent     text,
    title      text NOT NULL DEFAULT '',
    subtitle   text,
    icon       text,
    search     tsvector,
    owner_id   uuid,
    private    boolean NOT NULL DEFAULT false,
    plugin     text,
    collection text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz,
    PRIMARY KEY (scheme, id)
);

CREATE UNIQUE INDEX entities_uri ON rookery.entities (uri);
CREATE INDEX entities_parent ON rookery.entities (parent);
CREATE INDEX entities_space ON rookery.entities (space_id);
CREATE INDEX entities_owner ON rookery.entities (owner_id) WHERE owner_id IS NOT NULL;
CREATE INDEX entities_search ON rookery.entities USING gin (search);
CREATE INDEX entities_collection_id ON rookery.entities (plugin, collection, id);
CREATE INDEX entities_updated ON rookery.entities (updated_at DESC) WHERE deleted_at IS NULL;

-- A URI that now names another entity. Resolving, access and backlinks follow it.
CREATE TABLE rookery.entity_aliases (
    uri        text PRIMARY KEY,
    target     text NOT NULL,
    plugin     text,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX entity_aliases_target ON rookery.entity_aliases (target);

-- References between entities: ref and refs fields, and mentions inside rich text.
CREATE TABLE rookery.refs (
    src_scheme  text NOT NULL,
    src_id      text NOT NULL,
    field       text NOT NULL,
    dst_scheme  text NOT NULL,
    dst_id      text NOT NULL,
    -- The resource whose view level decides whether a caller sees the reference: the source entity,
    -- or for a row that is no entity, the row's container.
    access_uri  text NOT NULL,
    -- For rows of user-scoped collections that are no entities: only that person sees them.
    owner_id    uuid,
    plugin      text,
    collection  text,
    src_deleted boolean NOT NULL DEFAULT false,
    -- The target does not exist (any more).
    dangling    boolean NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (src_scheme, src_id, field, dst_scheme, dst_id)
);

CREATE INDEX refs_dst ON rookery.refs (dst_scheme, dst_id);
CREATE INDEX refs_collection ON rookery.refs (plugin, collection);

-- Spaces, such as projects. Membership is a grant on space://<id>.
CREATE TABLE rookery.spaces (
    id          uuid PRIMARY KEY,
    kind        text NOT NULL,
    name        text NOT NULL,
    slug        text NOT NULL UNIQUE,
    owner_id    uuid REFERENCES rookery.users (id) ON DELETE SET NULL,
    personal    boolean NOT NULL DEFAULT false,
    data        jsonb NOT NULL DEFAULT '{}',
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    archived_at timestamptz
);

CREATE UNIQUE INDEX spaces_personal ON rookery.spaces (owner_id) WHERE personal;

-- The change log of every collection. seq is given under an advisory lock just before commit, so it
-- is in commit order with no gaps. change_counter holds the last seq and how far the log was trimmed.
CREATE TABLE rookery.changes (
    seq        bigint PRIMARY KEY,
    plugin     text NOT NULL,
    collection text NOT NULL,
    id         uuid NOT NULL,
    op         text NOT NULL,
    rev        bigint NOT NULL,
    space_id   uuid,
    user_id    uuid,
    at         timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX changes_at ON rookery.changes (at);
CREATE INDEX changes_space_seq ON rookery.changes (space_id, seq DESC);

CREATE TABLE rookery.change_counter (
    singleton       boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    seq             bigint NOT NULL DEFAULT 0,
    trimmed_through bigint NOT NULL DEFAULT 0
);

INSERT INTO rookery.change_counter DEFAULT VALUES;

-- Field by field history of collections with history on.
CREATE TABLE rookery.history (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    plugin       text NOT NULL,
    collection   text NOT NULL,
    row_id       uuid NOT NULL,
    rev          bigint NOT NULL,
    op           text NOT NULL,
    -- {"field": [from, to], ...}
    changes      jsonb NOT NULL,
    principal    text NOT NULL,
    on_behalf_of uuid,
    at           timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX history_row ON rookery.history (plugin, collection, row_id, id DESC);

-- The collections each plugin's tables were last reconciled with, its schema version (the last
-- numbered migration that ran) and the counters of sequence fields. The plugin rookery is the server's
-- own collections.
CREATE TABLE rookery.plugin_schemas (
    plugin      text PRIMARY KEY,
    version     bigint NOT NULL DEFAULT 0,
    collections jsonb NOT NULL DEFAULT '[]',
    updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE rookery.plugin_migrations (
    plugin     text NOT NULL,
    version    bigint NOT NULL,
    applied_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plugin, version)
);

CREATE TABLE rookery.sequences (
    plugin     text NOT NULL,
    collection text NOT NULL,
    field      text NOT NULL,
    scope_key  text NOT NULL DEFAULT '',
    value      bigint NOT NULL,
    PRIMARY KEY (plugin, collection, field, scope_key)
);

-- Writes that carried an idempotency key: the key, a hash of the request and what it answered,
-- recorded in the write's transaction, so that a repeat answers the same without writing again.
CREATE TABLE rookery.idempotency (
    -- Whose key it is, such as plugin:<id>.
    scope        text NOT NULL,
    key          text NOT NULL,
    request_hash text NOT NULL,
    response     jsonb NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    PRIMARY KEY (scope, key)
);

CREATE INDEX idempotency_expires ON rookery.idempotency (expires_at);

-- The principals a caller acts as: itself, its user, the user's groups and everyone.
CREATE FUNCTION rookery.principals_of(p_user uuid, p_principal text) RETURNS text[]
LANGUAGE sql STABLE AS $$
    SELECT array_remove(ARRAY[
               p_principal,
               CASE WHEN p_user IS NULL THEN NULL ELSE 'user:' || p_user::text END,
               CASE WHEN p_principal LIKE 'plugin:%' THEN NULL ELSE 'everyone' END
           ], NULL)
        || coalesce((SELECT array_agg('group:' || m.group_id::text)
                     FROM rookery.group_members m WHERE m.user_id = p_user), '{}'::text[])
$$;

-- The highest level the principals hold on a resource: grants on it and on its ancestors (walking
-- parents in the entity index until a private entity), and manage for the owner of any of them. The
-- cost is high so that the planner checks cheaper conditions first.
CREATE FUNCTION rookery.effective_level(p_uri text, p_user uuid, p_principals text[]) RETURNS smallint
LANGUAGE sql STABLE COST 1000 AS $$
    WITH RECURSIVE chain (uri, depth) AS (
        SELECT p_uri, 0
        UNION ALL
        SELECT e.parent, c.depth + 1
        FROM chain c JOIN rookery.entities e ON e.uri = c.uri
        WHERE e.parent IS NOT NULL AND NOT e.private AND c.depth < 16
    )
    SELECT coalesce(max(l), 0)::smallint FROM (
        SELECT g.level AS l FROM chain c JOIN rookery.grants g ON g.resource = c.uri
        WHERE g.principal = ANY (p_principals)
        UNION ALL
        SELECT 3 FROM chain c JOIN rookery.entities e ON e.uri = c.uri
        WHERE p_user IS NOT NULL AND e.owner_id = p_user
        UNION ALL
        SELECT 3 FROM chain c JOIN rookery.owners o ON o.resource = c.uri
        WHERE p_user IS NOT NULL AND o.user_id = p_user
    ) s
$$;

-- The top-level containers (spaces, plugins) the principals hold a level on. Containers have no
-- parent, so these are their grants and ownership. An entity that is not private and whose parent is
-- one of them is visible without walking its chain.
CREATE FUNCTION rookery.visible_roots(p_user uuid, p_principals text[]) RETURNS SETOF text
LANGUAGE sql STABLE AS $$
    SELECT g.resource FROM rookery.grants g
    WHERE g.principal = ANY (p_principals) AND (g.resource LIKE 'space://%' OR g.resource LIKE 'plugin://%')
    UNION
    SELECT e.uri FROM rookery.entities e
    WHERE p_user IS NOT NULL AND e.owner_id = p_user AND e.scheme IN ('space', 'plugin')
$$;

-- Every entity whose level may be above zero for the principals: those granted to them or owned by
-- their user, and everything that inherits from those. It narrows a scan before the check of each row.
CREATE FUNCTION rookery.reachable(p_user uuid, p_principals text[]) RETURNS SETOF text
LANGUAGE sql STABLE AS $$
    WITH RECURSIVE f (uri) AS (
        SELECT g.resource FROM rookery.grants g WHERE g.principal = ANY (p_principals)
        UNION
        SELECT e.uri FROM rookery.entities e WHERE p_user IS NOT NULL AND e.owner_id = p_user
        UNION
        SELECT o.resource FROM rookery.owners o WHERE p_user IS NOT NULL AND o.user_id = p_user
    ), d (uri, depth) AS (
        SELECT uri, 0 FROM f
        UNION
        SELECT c.uri, d.depth + 1 FROM d JOIN rookery.entities c ON c.parent = d.uri
        WHERE NOT c.private AND d.depth < 16
    )
    SELECT DISTINCT uri FROM d
$$;

-- Documents that people edit together. A document is a Yjs document: a compacted snapshot and the
-- updates since, one row per update. Its entity is document://<id>.
CREATE TABLE rookery.documents (
    id           uuid PRIMARY KEY,
    plugin       text,
    parent       text,
    space_id     uuid,
    created_by   uuid,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    -- The principal of the last update.
    updated_by   text,
    -- The title given at creation; the entity's title follows the content once it has some.
    title        text NOT NULL DEFAULT '',
    -- The Yjs state (update format v1) that holds every update up to snapshot_seq.
    snapshot     bytea,
    snapshot_seq bigint NOT NULL DEFAULT 0,
    indexed_at   timestamptz
);

CREATE INDEX documents_parent ON rookery.documents (parent);

CREATE TABLE rookery.document_updates (
    document_id uuid NOT NULL REFERENCES rookery.documents (id) ON DELETE CASCADE,
    seq         bigint NOT NULL,
    data        bytea NOT NULL,
    actor       text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (document_id, seq)
);

-- Saved states for the history: named ones, periodic ones, and the state before a restore.
CREATE TABLE rookery.document_versions (
    id          uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES rookery.documents (id) ON DELETE CASCADE,
    kind        text NOT NULL CHECK (kind IN ('named', 'auto', 'restore')),
    name        text,
    data        bytea NOT NULL,
    created_by  text,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX document_versions_by_document ON rookery.document_versions (document_id, created_at DESC);

-- What a person opened and ran last, and pins: a person's own, or a space's shared ones.
CREATE TABLE rookery.recents (
    user_id   uuid NOT NULL REFERENCES rookery.users (id) ON DELETE CASCADE,
    kind      text NOT NULL CHECK (kind IN ('entity', 'command')),
    -- An entity's URI, or a command's id.
    key       text NOT NULL,
    opened_at timestamptz NOT NULL DEFAULT now(),
    count     integer NOT NULL DEFAULT 1,
    PRIMARY KEY (user_id, kind, key)
);

CREATE INDEX recents_recent ON rookery.recents (user_id, kind, opened_at DESC);

CREATE TABLE rookery.pins (
    id          uuid PRIMARY KEY,
    -- A personal pin has a user; a shared pin the space it is shared in. Exactly one of the two.
    user_id     uuid REFERENCES rookery.users (id) ON DELETE CASCADE,
    space_id    uuid,
    created_by  uuid,
    kind        text NOT NULL CHECK (kind IN ('entity', 'view')),
    -- An entity's URI, or a view's id.
    target      text NOT NULL,
    -- The path inside a view.
    path        text NOT NULL DEFAULT '',
    -- The space a pinned view opens in. Entities open in their own.
    view_space  uuid,
    title       text NOT NULL DEFAULT '',
    icon        text,
    position    bigint NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    CHECK ((user_id IS NULL) <> (space_id IS NULL))
);

CREATE UNIQUE INDEX pins_user_target ON rookery.pins (user_id, kind, target, path) WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX pins_space_target ON rookery.pins (space_id, kind, target, path) WHERE space_id IS NOT NULL;
CREATE INDEX pins_space ON rookery.pins (space_id) WHERE space_id IS NOT NULL;

-- Settings are a collection of Rookery's too, and a row of one needs an id.
ALTER TABLE rookery.settings ADD COLUMN id uuid NOT NULL DEFAULT gen_random_uuid();
CREATE UNIQUE INDEX settings_id ON rookery.settings (id);
