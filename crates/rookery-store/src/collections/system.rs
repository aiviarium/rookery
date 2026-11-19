//! Rookery's own data as collections of the plugin `rookery`, in the schema `rookery`.
//!
//! There are two kinds. A *native* system collection is declared like a plugin's ([`native`]) and its
//! table is made and changed by [`super::Collections::apply`]: Rookery's storage and layouts are. A
//! *typed* table, one with columns and code of its own such as `rookery.jobs`, is registered with a
//! [`SystemTable`]: it is then a collection like the others for reads (`find`, `get`, `count`,
//! `aggregate`, live queries, history), with the same scoping and access rules, while its writes stay
//! with its own code.
//!
//! A typed table is read through a view, `rookery."v_<name>"`, that gives it the columns every
//! collection has (`id`, `space_id`, `user_id`, `created_at`, `updated_at`, `rev`, ...), its other
//! columns as fields named in camel case, and `_resource` and `_owner`, which its rows' access is
//! checked on. Its writes reach the change log and history through a trigger that runs at commit,
//! whichever code made them, and that tells the change feed with `NOTIFY rookery_changes`.

use std::sync::Arc;

use serde_json::json;
use sqlx::PgConnection;

use super::schema::{AccessDef, CollectionDef, FieldDef, FieldType, Scope};
use crate::caller::Level;
use crate::error::{Error, Result};
use crate::util::quote_ident;

/// The plugin Rookery's own collections belong to. No plugin may take its name.
pub const PLUGIN: &str = "rookery";

/// The channel the trigger of typed tables tells the change feed on.
pub const CHANNEL: &str = "rookery_changes";

/// A typed table of Rookery's, as a collection.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemTable {
    /// The collection's name: `rookery/<name>`.
    pub name: &'static str,
    /// The table in the schema `rookery`.
    pub table: &'static str,
    /// The column that names a row. A UUID, or 32 hex digits of one, is the row's id as it is.
    pub id: &'static str,
    /// The id is a text key, such as a name: the row's id is a UUID made from it, and the key is a
    /// field.
    pub hashed_id: bool,
    /// `Global`, or `User` with [`SystemTable::user`]: a person's own rows.
    pub scope: Scope,
    /// The column of the person a row of the user scope is of.
    pub user: Option<&'static str>,
    /// SQL over the table's columns (as `t`) for `createdAt` and `updatedAt`.
    pub created_at: &'static str,
    pub updated_at: &'static str,
    /// SQL over the table's columns (as `t`) for the URI a row's access is checked on, such as its
    /// subject `env://NAME`. Callers see the rows they hold the read level on.
    pub resource: Option<&'static str>,
    /// SQL over the table's columns for a person who holds manage on the row, such as who enqueued it.
    pub owner: Option<&'static str>,
    /// Columns that are not fields: sealed values, hashes of tokens.
    pub omit: &'static [&'static str],
    /// Only admins read it, through `data.*`.
    pub admin: bool,
    /// Its changes are kept field by field.
    pub history: bool,
}

impl SystemTable {
    fn view(&self) -> String {
        format!("rookery.{}", quote_ident(&format!("v_{}", self.name)))
    }

    fn check(&self) -> Result<()> {
        let private = self.admin
            || self.scope == Scope::User
            || self.resource.is_some()
            || self.owner.is_some();
        if !private {
            return Err(Error::internal(format!(
                "rookery/{}: a typed table is for admins, a person's own, or checked on a resource",
                self.name
            )));
        }
        if (self.scope == Scope::User) != self.user.is_some() || self.scope == Scope::Space {
            return Err(Error::internal(format!(
                "rookery/{}: a typed table is global, or a person's with its user column",
                self.name
            )));
        }
        Ok(())
    }
}

/// The typed tables of Rookery's that are collections. Those of hosts join them from `rookery-hosts`
/// through [`crate::StoreOptions::system_tables`].
pub fn tables() -> Vec<SystemTable> {
    let principal_user =
        "CASE WHEN t.enqueued_by ~ '^user:[0-9a-f-]{36}$' THEN substr(t.enqueued_by, 6)::uuid END";
    vec![
        SystemTable {
            name: "users",
            table: "users",
            id: "id",
            hashed_id: false,
            scope: Scope::Global,
            user: None,
            created_at: "t.created_at",
            updated_at: "coalesce(t.last_seen_at, t.created_at)",
            resource: None,
            owner: None,
            omit: &[],
            admin: true,
            history: true,
        },
        SystemTable {
            name: "groups",
            table: "groups",
            id: "id",
            hashed_id: false,
            scope: Scope::Global,
            user: None,
            created_at: "t.created_at",
            updated_at: "t.created_at",
            resource: None,
            owner: None,
            omit: &[],
            admin: true,
            history: true,
        },
        SystemTable {
            name: "spaces",
            table: "spaces",
            id: "id",
            hashed_id: false,
            scope: Scope::Global,
            user: None,
            created_at: "t.created_at",
            updated_at: "t.updated_at",
            resource: Some("'space://' || t.id::text"),
            owner: Some("t.owner_id"),
            omit: &[],
            admin: false,
            history: true,
        },
        SystemTable {
            name: "jobs",
            table: "jobs",
            id: "id",
            hashed_id: false,
            scope: Scope::Global,
            user: None,
            created_at: "t.created_at",
            updated_at: "coalesce(t.finished_at, t.started_at, t.created_at)",
            resource: Some("t.subject"),
            owner: Some(principal_user),
            omit: &[],
            admin: false,
            history: false,
        },
        SystemTable {
            name: "notifications",
            table: "notifications",
            id: "id",
            hashed_id: false,
            scope: Scope::User,
            user: Some("user_id"),
            created_at: "t.created_at",
            updated_at: "coalesce(t.answered_at, t.read_at, t.created_at)",
            resource: None,
            owner: None,
            omit: &[],
            admin: false,
            history: false,
        },
        SystemTable {
            name: "connections",
            table: "connections",
            id: "id",
            hashed_id: false,
            scope: Scope::Global,
            user: None,
            created_at: "t.created_at",
            updated_at: "t.updated_at",
            resource: None,
            owner: None,
            omit: &["key_id", "nonce", "ciphertext"],
            admin: true,
            history: true,
        },
        SystemTable {
            name: "blobs",
            table: "blobs",
            id: "id",
            hashed_id: false,
            scope: Scope::Global,
            user: None,
            created_at: "t.created_at",
            updated_at: "t.created_at",
            resource: Some("'blob://' || t.id::text"),
            owner: Some(
                "CASE WHEN t.owner ~ '^user:[0-9a-f-]{36}$' THEN substr(t.owner, 6)::uuid END",
            ),
            omit: &[],
            admin: false,
            history: false,
        },
        SystemTable {
            name: "settings",
            table: "settings",
            id: "id",
            hashed_id: false,
            scope: Scope::Global,
            user: None,
            created_at: "t.updated_at",
            updated_at: "t.updated_at",
            resource: None,
            owner: None,
            omit: &[],
            admin: true,
            history: true,
        },
        SystemTable {
            name: "audit",
            table: "audit",
            id: "id",
            hashed_id: true,
            scope: Scope::Global,
            user: None,
            created_at: "t.at",
            updated_at: "t.at",
            resource: None,
            owner: None,
            omit: &[],
            admin: true,
            history: false,
        },
    ]
}

/// Rookery's native collections: key-value storage at each scope and the layouts of the shell.
pub fn native() -> Vec<CollectionDef> {
    let storage = |name: &str, scope: &str| {
        json!({
            "name": name,
            "scope": scope,
            "access": { "read": "manage", "write": "manage", "admin": true },
            "fields": [
                { "name": "plugin", "type": "text", "required": true, "index": true },
                { "name": "key", "type": "text", "required": true, "maxLength": 500 },
                { "name": "value", "type": "json" },
                // Every set counts, so that each gives the key a new revision, as storage promises.
                { "name": "writes", "type": "integer", "default": 0 }
            ],
            "unique": [["plugin", "key"]]
        })
    };
    serde_json::from_value(json!([
        storage("storage", "global"),
        storage("storageUser", "user"),
        storage("storageSpace", "space"),
        {
            "name": "userLayouts",
            "scope": "user",
            "fields": [
                { "name": "name", "type": "text", "required": true, "unique": true, "maxLength": 200 },
                { "name": "value", "type": "json" }
            ]
        }
    ]))
    .expect("Rookery's own collections are valid")
}

/// `snake_case` to `camelCase`, as the fields of typed tables are named.
pub fn camel(column: &str) -> String {
    let mut out = String::with_capacity(column.len());
    let mut upper = false;
    for c in column.chars() {
        if c == '_' {
            upper = !out.is_empty();
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// The API names of the columns every collection has; a typed table's column of that name is left out.
const SYSTEM_NAMES: &[&str] = &[
    "id",
    "spaceId",
    "userId",
    "createdAt",
    "updatedAt",
    "createdBy",
    "updatedBy",
    "rev",
    "deletedAt",
];

/// The field and the view's expression of a column, or `None` for one that is no field.
fn field_of(column: &str, ty: &str) -> Option<(FieldType, String)> {
    let col = format!("t.{}", quote_ident(column));
    Some(match ty {
        "text" | "character varying" | "character" => (FieldType::Text, col),
        "uuid" => (FieldType::Text, format!("{col}::text")),
        "jsonb" | "json" => (FieldType::Json, col),
        "timestamp with time zone" => (FieldType::Datetime, col),
        "boolean" => (FieldType::Boolean, col),
        "smallint" | "integer" | "bigint" => (FieldType::Integer, col),
        "double precision" | "real" | "numeric" => {
            (FieldType::Number, format!("{col}::double precision"))
        }
        _ => return None,
    })
}

/// Makes the view and the triggers of a typed table, and returns its collection.
pub(crate) async fn register(conn: &mut PgConnection, t: &SystemTable) -> Result<CollectionDef> {
    t.check()?;
    let columns: Vec<(String, String)> = sqlx::query_as(
        "SELECT column_name::text, data_type::text FROM information_schema.columns
         WHERE table_schema = 'rookery' AND table_name = $1 ORDER BY ordinal_position",
    )
    .bind(t.table)
    .fetch_all(&mut *conn)
    .await?;
    if columns.is_empty() {
        return Err(Error::internal(format!("rookery.{} is no table", t.table)));
    }
    let table = format!("rookery.{}", quote_ident(t.table));
    if !columns.iter().any(|(c, _)| c == "rev") {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE {table} ADD COLUMN IF NOT EXISTS rev bigint NOT NULL DEFAULT 1"
        )))
        .execute(&mut *conn)
        .await?;
    }
    let mut fields = Vec::new();
    let mut selects = Vec::new();
    for (column, ty) in &columns {
        let api = camel(column);
        if column == "rev"
            || t.omit.contains(&column.as_str())
            || SYSTEM_NAMES.contains(&api.as_str())
        {
            continue;
        }
        let Some((ty, expr)) = field_of(column, ty) else {
            continue;
        };
        selects.push(format!("{expr} AS {}", quote_ident(&api)));
        fields.push(FieldDef::named(&api, ty));
    }
    let id_col = format!("t.{}", quote_ident(t.id));
    let id = if t.hashed_id {
        format!("md5({id_col}::text)::uuid")
    } else {
        format!("{id_col}::text::uuid")
    };
    let user = match t.user {
        Some(u) => format!("t.{}", quote_ident(u)),
        None => "NULL::uuid".into(),
    };
    let view = t.view();
    let sql = format!(
        "CREATE VIEW {view} AS SELECT {id} AS id, NULL::uuid AS space_id, {user} AS user_id,
             ({created}) AS created_at, ({updated}) AS updated_at, NULL::uuid AS created_by,
             NULL::uuid AS updated_by, t.rev AS rev, NULL::timestamptz AS deleted_at, NULL::tsvector AS _search,
             {fields}{comma} ({resource})::text AS _resource, ({owner})::uuid AS _owner
         FROM {table} t",
        created = t.created_at,
        updated = t.updated_at,
        fields = selects.join(", "),
        comma = if selects.is_empty() { "" } else { "," },
        resource = t.resource.unwrap_or("NULL"),
        owner = t.owner.unwrap_or("NULL"),
    );
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP VIEW IF EXISTS {view}")))
        .execute(&mut *conn)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .execute(&mut *conn)
        .await?;
    for (trigger, sql) in [
        (
            "rookery_rev",
            format!("CREATE TRIGGER rookery_rev BEFORE UPDATE ON {table} FOR EACH ROW EXECUTE FUNCTION rookery.bump_rev()"),
        ),
        (
            "rookery_capture",
            format!(
                "CREATE CONSTRAINT TRIGGER rookery_capture AFTER INSERT OR UPDATE OR DELETE ON {table}
                 DEFERRABLE INITIALLY DEFERRED FOR EACH ROW
                 EXECUTE FUNCTION rookery.capture('{PLUGIN}', '{name}', '{id}', '{hashed}', '{user}', '{history}', '{omit}')",
                name = t.name,
                id = t.id,
                hashed = if t.hashed_id { 1 } else { 0 },
                user = t.user.unwrap_or(""),
                history = if t.history { 1 } else { 0 },
                omit = t.omit.join(","),
            ),
        ),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TRIGGER IF EXISTS {trigger} ON {table}")))
            .execute(&mut *conn)
            .await?;
        sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&mut *conn).await?;
    }
    let mut def = CollectionDef::named(t.name, t.scope, fields);
    def.history = t.history;
    def.access = AccessDef {
        read: Level::View,
        write: Level::Manage,
        delete: None,
        admin: t.admin,
    };
    def.validate()?;
    Ok(def)
}

/// The functions the triggers of typed tables run. Made at every start, so that they follow this build.
pub(crate) async fn functions(conn: &mut PgConnection) -> Result<()> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        r#"
CREATE OR REPLACE FUNCTION rookery.camel(name text) RETURNS text LANGUAGE sql IMMUTABLE AS $f$
    SELECT string_agg(CASE WHEN i = 1 THEN p ELSE upper(left(p, 1)) || substr(p, 2) END, '' ORDER BY i)
    FROM unnest(string_to_array(name, '_')) WITH ORDINALITY AS u(p, i) WHERE p <> ''
$f$;

CREATE OR REPLACE FUNCTION rookery.bump_rev() RETURNS trigger LANGUAGE plpgsql AS $f$
BEGIN
    IF NEW.rev = OLD.rev THEN
        NEW.rev := OLD.rev + 1;
    END IF;
    RETURN NEW;
END
$f$;

-- At commit, for each row a statement wrote: the change log entry, in commit order under the lock the
-- collections take, the history when the table keeps it, and a notice to the change feed.
-- Arguments: plugin, collection, id column, whether the id is hashed, user column, history, omitted columns.
CREATE OR REPLACE FUNCTION rookery.capture() RETURNS trigger LANGUAGE plpgsql AS $f$
DECLARE
    before jsonb := CASE WHEN TG_OP <> 'INSERT' THEN to_jsonb(OLD) END;
    after jsonb := CASE WHEN TG_OP <> 'DELETE' THEN to_jsonb(NEW) END;
    r jsonb := coalesce(after, before);
    key text := r ->> TG_ARGV[2];
    rid uuid := CASE WHEN TG_ARGV[3] = '1' THEN md5(key)::uuid ELSE key::uuid END;
    uid uuid := CASE WHEN TG_ARGV[4] <> '' THEN (r ->> TG_ARGV[4])::uuid END;
    omit text[] := string_to_array(TG_ARGV[6], ',');
    rev bigint := coalesce((r ->> 'rev')::bigint, 1);
    s bigint;
    diff jsonb;
    actor text := coalesce(nullif(current_setting('rookery.actor', true), ''), 'system:rookery');
BEGIN
    PERFORM pg_advisory_xact_lock({lock});
    UPDATE rookery.change_counter SET seq = seq + 1 RETURNING seq INTO s;
    INSERT INTO rookery.changes (seq, plugin, collection, id, op, rev, space_id, user_id)
    VALUES (s, TG_ARGV[0], TG_ARGV[1], rid, lower(TG_OP), rev, NULL, uid);
    IF TG_ARGV[5] = '1' THEN
        SELECT coalesce(jsonb_object_agg(rookery.camel(k), jsonb_build_array(before -> k, after -> k)), '{{}}')
        INTO diff
        FROM (SELECT jsonb_object_keys(coalesce(before, '{{}}') || coalesce(after, '{{}}')) AS k) keys
        WHERE k <> 'rev' AND NOT (k = ANY (coalesce(omit, '{{}}')))
          AND (before -> k) IS DISTINCT FROM (after -> k);
        IF TG_OP <> 'UPDATE' OR diff <> '{{}}' THEN
            INSERT INTO rookery.history (plugin, collection, row_id, rev, op, changes, principal)
            VALUES (TG_ARGV[0], TG_ARGV[1], rid, rev, lower(TG_OP), diff, actor);
        END IF;
    END IF;
    PERFORM pg_notify('{channel}', s::text);
    RETURN NULL;
END
$f$;
"#,
        lock = crate::feed::SEQ_LOCK,
        channel = CHANNEL,
    )))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Registers every typed table and applies the native collections, under one lock so that two stores
/// starting on one database do not both make the views.
pub(crate) async fn install(
    store: &crate::Store,
    tables: &[SystemTable],
) -> Result<Vec<(Arc<SystemTable>, CollectionDef)>> {
    let mut tx = store.pool().begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('rookery.system-collections'))")
        .execute(&mut *tx)
        .await?;
    functions(&mut tx).await?;
    let mut out = Vec::new();
    for t in tables {
        let def = register(&mut tx, t).await?;
        out.push((Arc::new(t.clone()), def));
    }
    tx.commit().await?;
    Ok(out)
}

/// Moves what the tables `rookery.kv` and `rookery.layouts` held into Rookery's native collections,
/// once, and drops the tables.
pub(crate) async fn adopt_tables(store: &crate::Store) -> Result<()> {
    let mut tx = store.pool().begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('rookery.system-collections'))")
        .execute(&mut *tx)
        .await?;
    let exists = |name: &'static str| {
        sqlx::query_scalar::<_, Option<String>>("SELECT to_regclass($1)::text").bind(name)
    };
    if exists("rookery.kv").fetch_one(&mut *tx).await?.is_some() {
        for (scope, table) in [("global", "storage"), ("user", "\"storageUser\"")] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO rookery.{table} (id, user_id, created_at, updated_at, rev, plugin, key, value)
                 SELECT gen_random_uuid(), user_id, updated_at, updated_at, revision, plugin, key, value
                 FROM rookery.kv WHERE scope = '{scope}'"
            )))
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("DROP TABLE rookery.kv")
            .execute(&mut *tx)
            .await?;
    }
    if exists("rookery.layouts")
        .fetch_one(&mut *tx)
        .await?
        .is_some()
    {
        sqlx::query(
            "INSERT INTO rookery.\"userLayouts\" (id, user_id, created_at, updated_at, rev, name, value)
             SELECT gen_random_uuid(), user_id, updated_at, updated_at, revision, name, value FROM rookery.layouts",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query("DROP TABLE rookery.layouts")
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}
