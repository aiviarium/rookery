//! Plugin collections: definitions, tables and their additive migration, numbered plugin
//! migrations, structured queries and writes.

pub mod aggregate;
mod bulk;
pub mod conditions;
pub mod constraints;
mod ddl;
pub mod filter;
mod include;
pub mod log;
mod migrations;
pub mod nearest;
pub mod operations;
mod projection;
mod query;
pub mod richtext;
pub mod schema;
pub mod system;
pub mod values;
mod write;

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Map, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

pub use aggregate::{AggregateQuery, Bucket, GroupBy, Measure};
pub use bulk::{ImportFailure, ImportMode, ImportOptions, ImportOutcome, MAX_IMPORT_ROWS};
pub use filter::Filter;
pub use include::MAX_INCLUDE;
pub use log::{MaintainReport, Tail, TailQuery};
pub use migrations::{
    ExtraFields, MigrationStep, MigrationTx, NoTransforms, PluginMigration, Transformer,
};
pub use nearest::{MAX_NEAREST_LIMIT, Nearest};
pub use query::{DEFAULT_LIMIT, Direction, MAX_LIMIT, OrderBy, Page, Query};
pub use schema::{
    AccessDef, CollectionDef, CollectionKind, EntityDef, EnumTone, EnumValue, FieldDef,
    FieldFormat, FieldIndex, FieldType, Partition, Retention, Scope, VectorIndex, plugin_schema,
    validate_plugin_id,
};
pub use write::{BatchJob, BatchOp, BatchOutcome, Mention, WriteOutcome};

use crate::Store;
use crate::caller::{Caller, Level};
use crate::entities::EntityRecord;
use crate::error::{Error, Result, SchemaDifference};
use crate::util::quote_ident;
use values::{Kind, row_out};
use write::WriteTx;

/// The largest message a batch publishes to a topic.
pub const MAX_TOPIC_PAYLOAD: usize = 256 * 1024;

/// Schemes the server uses itself; collections cannot claim them.
const KERNEL_SCHEMES: &[&str] = &[
    "user",
    "group",
    "space",
    "env",
    "terminal",
    "session",
    "job",
    "document",
    "blob",
    "connection",
    "plugin",
    "row",
];

/// A collection of a plugin, resolved: its definition and table.
#[derive(Debug, Clone)]
pub struct Collection {
    pub plugin: String,
    pub def: CollectionDef,
    pub(crate) schema: String,
    /// `"p_plugin"."name"`, or the view of a typed table of Rookery's.
    pub(crate) table: String,
    /// A typed table of Rookery's: read through its view, written by its own code.
    pub(crate) system: Option<Arc<system::SystemTable>>,
}

impl Collection {
    pub(crate) fn new(plugin: &str, def: CollectionDef) -> Collection {
        let schema = plugin_schema(plugin);
        let table = format!("{}.{}", quote_ident(&schema), quote_ident(&def.name));
        Collection {
            plugin: plugin.to_string(),
            def,
            schema,
            table,
            system: None,
        }
    }

    /// A typed table of Rookery's, read through its view.
    pub(crate) fn system(t: Arc<system::SystemTable>, def: CollectionDef) -> Collection {
        Collection {
            plugin: system::PLUGIN.to_string(),
            def,
            schema: system::PLUGIN.to_string(),
            table: format!("rookery.{}", quote_ident(&format!("v_{}", t.name))),
            system: Some(t),
        }
    }

    /// Whether it is a typed table of Rookery's, which `data.*` reads and does not write.
    pub fn is_typed(&self) -> bool {
        self.system.is_some()
    }

    /// The entity URI of a row, for entity collections.
    pub fn entity_uri(&self, id: Uuid) -> Option<String> {
        self.def
            .entity
            .as_ref()
            .map(|e| format!("{}://{id}", e.scheme))
    }

    pub fn is_log(&self) -> bool {
        self.def.is_log()
    }

    pub(crate) fn column(&self, name: &str) -> Option<(String, Kind)> {
        query::column(self, name)
    }
}

/// The resource a row's access is checked on when it is not an entity (and an entity row's
/// default parent): its space, its user, or the plugin for global collections.
pub(crate) fn container_uri(coll: &Collection, space: Option<Uuid>, user: Option<Uuid>) -> String {
    match (coll.def.scope, space, user) {
        (Scope::Space, Some(s), _) => crate::uri::space_uri(s),
        (Scope::User, _, Some(u)) => crate::uri::user_uri(u),
        _ => crate::uri::plugin_uri(&coll.plugin),
    }
}

/// What [`Collections::apply`] did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyReport {
    pub created: Vec<String>,
    /// `collection: change` lines.
    pub altered: Vec<String>,
}

/// Collection definitions and their tables.
pub struct Collections<'a> {
    pub(crate) store: &'a Store,
}

impl<'a> Collections<'a> {
    /// Loads every stored definition into memory.
    pub async fn reload(&self) -> Result<()> {
        // Every apply commits and then reloads; serialised, the last reload to run reads after every
        // commit that came before it, so no plugin's collections are lost to a stale read.
        let _serial = self.store.data.reload_lock.lock().await;
        let rows = sqlx::query("SELECT plugin, collections FROM rookery.plugin_schemas")
            .fetch_all(self.store.pool())
            .await?;
        let mut map = HashMap::new();
        for r in rows {
            let plugin: String = r.try_get("plugin")?;
            let defs: Vec<CollectionDef> = serde_json::from_value(r.try_get("collections")?)
                .map_err(|e| Error::internal(format!("stored schema of {plugin}: {e}")))?;
            for d in defs {
                map.insert(
                    (plugin.clone(), d.name.clone()),
                    Arc::new(Collection::new(&plugin, d)),
                );
            }
        }
        for (t, def) in self
            .store
            .data
            .system
            .read()
            .expect("system tables lock")
            .iter()
        {
            map.insert(
                (system::PLUGIN.to_string(), def.name.clone()),
                Arc::new(Collection::system(t.clone(), def.clone())),
            );
        }
        *self.store.data.registry.write().expect("registry lock") = map;
        Ok(())
    }

    /// Makes Rookery's own collections: registers its typed tables and applies its native collections,
    /// moving what the old key-value and layout tables held into them.
    pub async fn install_system(&self, extra: &[system::SystemTable]) -> Result<()> {
        let mut tables = system::tables();
        tables.extend(extra.iter().cloned());
        let native = system::native();
        if let Some(t) = tables
            .iter()
            .find(|t| native.iter().any(|n| n.name == t.name))
        {
            return Err(Error::internal(format!(
                "rookery/{} is declared twice",
                t.name
            )));
        }
        let registered = system::install(self.store, &tables).await?;
        *self.store.data.system.write().expect("system tables lock") = registered;
        self.apply(system::PLUGIN, &native).await?;
        system::adopt_tables(self.store).await?;
        self.reload().await
    }

    /// A collection by plugin and name.
    pub fn get(&self, plugin: &str, name: &str) -> Result<Arc<Collection>> {
        self.store
            .data
            .registry
            .read()
            .expect("registry lock")
            .get(&(plugin.to_string(), name.to_string()))
            .cloned()
            .ok_or_else(|| Error::not_found(format!("plugin {plugin} has no collection {name}")))
    }

    /// The collection whose rows are the entities of `scheme`, in whichever plugin declares it.
    pub fn by_scheme(&self, scheme: &str) -> Option<Arc<Collection>> {
        self.store
            .data
            .registry
            .read()
            .expect("registry lock")
            .values()
            .find(|c| c.def.entity.as_ref().is_some_and(|e| e.scheme == scheme))
            .cloned()
    }

    /// The collections of a plugin.
    pub fn list(&self, plugin: &str) -> Vec<Arc<Collection>> {
        let mut out: Vec<_> = self
            .store
            .data
            .registry
            .read()
            .expect("registry lock")
            .iter()
            .filter(|((p, _), _)| p == plugin)
            .map(|(_, c)| c.clone())
            .collect();
        out.sort_by(|a, b| a.def.name.cmp(&b.def.name));
        out
    }

    /// The plugin's schema version: the last numbered migration applied.
    pub async fn version(&self, plugin: &str) -> Result<i64> {
        Ok(
            sqlx::query_scalar("SELECT version FROM rookery.plugin_schemas WHERE plugin = $1")
                .bind(plugin)
                .fetch_optional(self.store.pool())
                .await?
                .unwrap_or(0),
        )
    }

    /// Reconciles the plugin's tables with its manifest: creates new collections, adds fields
    /// and indexes, drops indexes no longer declared, reindexes search and entities when their
    /// options change. Anything else fails with [`Error::Schema`] listing every difference,
    /// and nothing is changed. Run [`Collections::migrate`] first.
    pub async fn apply(&self, plugin: &str, defs: &[CollectionDef]) -> Result<ApplyReport> {
        validate_plugin_id(plugin)?;
        let mut names = std::collections::HashSet::new();
        let mut schemes = std::collections::HashSet::new();
        for d in defs {
            d.validate()?;
            if !names.insert(d.name.as_str()) {
                return Err(Error::invalid(format!(
                    "collection {} is declared twice",
                    d.name
                )));
            }
            if let Some(e) = &d.entity {
                if KERNEL_SCHEMES.contains(&e.scheme.as_str()) {
                    return Err(Error::invalid(format!(
                        "scheme {} is reserved by the kernel",
                        e.scheme
                    )));
                }
                if !schemes.insert(e.scheme.as_str()) {
                    return Err(Error::invalid(format!("scheme {} is used twice", e.scheme)));
                }
            }
        }
        if let Some((d, f)) = defs.iter().find_map(|d| {
            d.fields
                .iter()
                .find(|f| matches!(f.ty, FieldType::Vector { .. }))
                .map(|f| (d, f))
        }) {
            let context = || {
                format!(
                    "plugin {plugin}, collection {}, field {} requires pgvector >= 0.8",
                    d.name, f.name
                )
            };
            let mut extension = self.store.pool().begin().await?;
            // IF NOT EXISTS alone does not serialize concurrent extension installation.
            sqlx::query("SELECT pg_advisory_xact_lock(hashtext('rookery.extension.vector'))")
                .execute(&mut *extension)
                .await?;
            sqlx::query("CREATE EXTENSION IF NOT EXISTS vector SCHEMA public")
                .execute(&mut *extension)
                .await
                .map_err(|e| Error::invalid(format!("{}: {e}", context())))?;
            let version: String =
                sqlx::query_scalar("SELECT extversion FROM pg_extension WHERE extname = 'vector'")
                    .fetch_one(&mut *extension)
                    .await?;
            let parts: Vec<u32> = version.split('.').filter_map(|p| p.parse().ok()).collect();
            if parts.as_slice() < [0, 8].as_slice() {
                return Err(Error::invalid(context()));
            }
            extension.commit().await?;
        }
        let mut tx = self.store.pool().begin().await?;
        lock_plugin(&mut tx, plugin).await?;
        let taken: Vec<(String, String)> = sqlx::query_as(
            "SELECT plugin, c->'entity'->>'scheme' FROM rookery.plugin_schemas, jsonb_array_elements(collections) c \
             WHERE plugin <> $1 AND c->'entity'->>'scheme' IS NOT NULL",
        )
        .bind(plugin)
        .fetch_all(&mut *tx)
        .await?;
        for (other, scheme) in taken {
            if schemes.contains(scheme.as_str()) {
                return Err(Error::conflict(format!(
                    "scheme {scheme} is already used by plugin {other}"
                )));
            }
        }
        let stored = load_stored(&mut tx, plugin).await?;
        let mut diffs: Vec<SchemaDifference> = ddl::removed_collections(&stored, defs);
        let mut plans = Vec::new();
        for d in defs {
            let new = Collection::new(plugin, d.clone());
            match stored.iter().find(|s| s.name == d.name) {
                None => plans.push((new, None)),
                Some(s) => match ddl::diff(&Collection::new(plugin, s.clone()), &new) {
                    Ok(plan) => plans.push((new, Some(plan))),
                    Err(mut d) => diffs.append(&mut d),
                },
            }
        }
        if !diffs.is_empty() {
            return Err(Error::Schema(diffs));
        }
        let mut report = ApplyReport::default();
        if stored.as_slice() == defs {
            return Ok(report);
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE SCHEMA IF NOT EXISTS {}",
            quote_ident(&plugin_schema(plugin))
        )))
        .execute(&mut *tx)
        .await?;
        ensure_plugin_entity(&mut tx, plugin).await?;
        for (coll, plan) in &plans {
            match plan {
                None => {
                    for sql in ddl::create_table(coll) {
                        sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
                            .execute(&mut *tx)
                            .await?;
                    }
                    if coll.def.is_log() {
                        log::create_partitions(&mut tx, coll, chrono::Utc::now()).await?;
                    }
                    report.created.push(coll.def.name.clone());
                }
                Some(plan) => {
                    for f in &plan.add_fields {
                        sqlx::query(sqlx::AssertSqlSafe(format!(
                            "ALTER TABLE {} ADD COLUMN {}",
                            coll.table,
                            ddl::column_sql(f)
                        )))
                        .execute(&mut *tx)
                        .await?;
                    }
                    for name in &plan.drop_indexes {
                        sqlx::query(sqlx::AssertSqlSafe(
                            ddl::drop_index_sql(coll, name).to_string(),
                        ))
                        .execute(&mut *tx)
                        .await?;
                    }
                    for s in &plan.create_indexes {
                        sqlx::query(sqlx::AssertSqlSafe(s.create.to_string()))
                            .execute(&mut *tx)
                            .await
                            .map_err(|e| {
                                let e = Error::from(e);
                                if e.is_unique_violation() {
                                    Error::conflict(format!(
                                        "{}: existing rows violate a new unique constraint",
                                        coll.def.name
                                    ))
                                } else {
                                    e
                                }
                            })?;
                    }
                    if plan.reindex_refs {
                        sqlx::query("DELETE FROM rookery.refs WHERE src_scheme = 'row' AND plugin = $1 AND collection = $2")
                            .bind(plugin)
                            .bind(&coll.def.name)
                            .execute(&mut *tx)
                            .await?;
                    }
                    if plan.reindex_search || plan.reindex_entities || plan.reindex_refs {
                        reindex(
                            &mut tx,
                            coll,
                            plan.reindex_search,
                            plan.reindex_entities,
                            plan.reindex_refs,
                        )
                        .await?;
                    }
                    report
                        .altered
                        .extend(plan.notes.iter().map(|n| format!("{}: {n}", coll.def.name)));
                }
            }
        }
        save_stored(&mut tx, plugin, defs).await?;
        tx.commit().await?;
        self.reload().await?;
        Ok(report)
    }

    /// Whether the plugin has never had a schema: nothing was installed before, or only an empty one.
    pub async fn is_fresh(&self, plugin: &str) -> Result<bool> {
        validate_plugin_id(plugin)?;
        let row: Option<(i64, Value)> = sqlx::query_as(
            "SELECT version, collections FROM rookery.plugin_schemas WHERE plugin = $1",
        )
        .bind(plugin)
        .fetch_optional(self.store.pool())
        .await?;
        Ok(match &row {
            None => true,
            Some((v, c)) => *v == 0 && c.as_array().is_none_or(|a| a.is_empty()),
        })
    }

    /// Records a plugin that has never stored a schema as being at `version` without running any
    /// migration: on a first install the manifest already describes the result of every numbered
    /// migration, so they are skipped. Returns whether it did (false when the plugin has a stored schema
    /// or a version already).
    pub async fn baseline(&self, plugin: &str, version: i64) -> Result<bool> {
        validate_plugin_id(plugin)?;
        if version <= 0 {
            return Ok(false);
        }
        let mut tx = self.store.pool().begin().await?;
        lock_plugin(&mut tx, plugin).await?;
        let row: Option<(i64, Value)> = sqlx::query_as(
            "SELECT version, collections FROM rookery.plugin_schemas WHERE plugin = $1 FOR UPDATE",
        )
        .bind(plugin)
        .fetch_optional(&mut *tx)
        .await?;
        let fresh = match &row {
            None => true,
            Some((v, c)) => *v == 0 && c.as_array().is_none_or(|a| a.is_empty()),
        };
        if !fresh {
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO rookery.plugin_schemas (plugin, collections, version) VALUES ($1, '[]'::jsonb, $2) \
             ON CONFLICT (plugin) DO UPDATE SET version = EXCLUDED.version, updated_at = now()",
        )
        .bind(plugin)
        .bind(version)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO rookery.plugin_migrations (plugin, version) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(plugin)
        .bind(version)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Moves the rows of the user-scope collection `from` into the space-scope `into`: each keeps its id and
    /// goes into its owner's personal space, with `extra(row)` added to its fields. Returns how many moved
    /// (none once they have, so a plugin may call it every time it loads).
    pub async fn move_user_rows(
        &self,
        plugin: &str,
        from: &str,
        into: &str,
        extra: &ExtraFields<'_>,
    ) -> Result<usize> {
        migrations::move_user_rows(self.store, plugin, from, into, extra).await
    }

    /// Runs the plugin's numbered migrations newer than its schema version, each in its own
    /// transaction. `transformer` runs the data-transform steps.
    pub async fn migrate(
        &self,
        plugin: &str,
        migrations: &[PluginMigration],
        transformer: &dyn Transformer,
    ) -> Result<Vec<i64>> {
        migrations::run(self.store, plugin, migrations, transformer).await
    }
}

pub(crate) async fn lock_plugin(conn: &mut PgConnection, plugin: &str) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('rookery.plugin:' || $1))")
        .bind(plugin)
        .execute(conn)
        .await?;
    Ok(())
}

pub(crate) async fn load_stored(
    conn: &mut PgConnection,
    plugin: &str,
) -> Result<Vec<CollectionDef>> {
    let v: Option<Value> = sqlx::query_scalar(
        "SELECT collections FROM rookery.plugin_schemas WHERE plugin = $1 FOR UPDATE",
    )
    .bind(plugin)
    .fetch_optional(&mut *conn)
    .await?;
    match v {
        Some(v) => serde_json::from_value(v)
            .map_err(|e| Error::internal(format!("stored schema of {plugin}: {e}"))),
        None => Ok(Vec::new()),
    }
}

pub(crate) async fn save_stored(
    conn: &mut PgConnection,
    plugin: &str,
    defs: &[CollectionDef],
) -> Result<()> {
    sqlx::query(
        "INSERT INTO rookery.plugin_schemas (plugin, collections) VALUES ($1, $2) \
         ON CONFLICT (plugin) DO UPDATE SET collections = EXCLUDED.collections, updated_at = now()",
    )
    .bind(plugin)
    .bind(serde_json::to_value(defs).map_err(|e| Error::internal(e.to_string()))?)
    .execute(conn)
    .await?;
    Ok(())
}

/// `plugin://<id>` is the container of global collections. Everyone gets `use` on it when it
/// is first registered; admins can narrow that with grants.
async fn ensure_plugin_entity(conn: &mut PgConnection, plugin: &str) -> Result<()> {
    let uri = crate::uri::plugin_uri(plugin);
    if crate::entities::fetch(&mut *conn, &uri).await?.is_some() {
        return Ok(());
    }
    crate::entities::upsert(
        &mut *conn,
        &EntityRecord {
            uri: uri.clone(),
            title: plugin.to_string(),
            plugin: Some(plugin.into()),
            ..Default::default()
        },
        None,
    )
    .await?;
    sqlx::query(
        "INSERT INTO rookery.grants (resource, principal, level, created_by) VALUES ($1, $2, $3, 'system:rookery') \
         ON CONFLICT DO NOTHING",
    )
    .bind(&uri)
    .bind(crate::caller::EVERYONE)
    .bind(Level::Use.as_i16())
    .execute(conn)
    .await?;
    Ok(())
}

/// Recomputes derived data for every row: the search vector, the entity index entry, and the
/// references.
pub(crate) async fn reindex(
    conn: &mut PgConnection,
    coll: &Collection,
    search: bool,
    entities: bool,
    refs: bool,
) -> Result<()> {
    let mut after = Uuid::nil();
    loop {
        let rows: Vec<Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT to_jsonb(t) - '_search' FROM {} t WHERE id > $1 ORDER BY id LIMIT 500",
            coll.table
        )))
        .bind(after)
        .fetch_all(&mut *conn)
        .await?;
        if rows.is_empty() {
            return Ok(());
        }
        for raw in rows {
            let row = row_out(coll, raw);
            after = write::row_id(&row)?;
            if search {
                write::update_search(&mut *conn, coll, &row).await?;
            }
            if entities || refs {
                let changed: Option<Vec<String>> = if refs { None } else { Some(Vec::new()) };
                write::index_row(&mut *conn, coll, &row, Some(&row), changed.as_deref()).await?;
                if row.get("deletedAt").is_some_and(|d| !d.is_null()) {
                    write::set_row_deleted(&mut *conn, coll, &row, true).await?;
                }
            }
        }
    }
}

/// Current rows by id (the feed's catch-up uses it).
pub(crate) async fn load_rows_by_id(
    store: &Store,
    coll: &Collection,
    ids: &[Uuid],
) -> Result<Vec<(Uuid, Value)>> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT id, to_jsonb(t) - '_search' AS row FROM {} t WHERE id = ANY($1)",
        coll.table
    )))
    .bind(ids)
    .fetch_all(store.pool())
    .await?;
    rows.into_iter()
        .map(|r| Ok((r.try_get("id")?, row_out(coll, r.try_get("row")?))))
        .collect()
}

/// One plugin's collections, for queries and writes. Every call takes the caller; the store
/// adds the scope filter and enforces the collection's access levels.
pub struct Data<'a> {
    pub(crate) store: &'a Store,
    pub(crate) plugin: &'a str,
}

impl<'a> Data<'a> {
    fn coll(&self, name: &str) -> Result<Arc<Collection>> {
        self.store.collections().get(self.plugin, name)
    }

    /// A collection `data.*` may write: not a typed table of Rookery's, which its own code writes.
    fn writable(&self, name: &str) -> Result<Arc<Collection>> {
        let coll = self.coll(name)?;
        if coll.is_typed() {
            return Err(Error::forbidden(format!(
                "rookery/{name} is written through its own methods"
            )));
        }
        Ok(coll)
    }

    /// A collection that rows can be changed in: not a log, which is only appended to and read.
    fn regular(&self, name: &str) -> Result<Arc<Collection>> {
        let coll = self.writable(name)?;
        if coll.is_log() {
            return Err(Error::invalid(format!(
                "{name} is a log collection: rows are only appended (insert, import) and read"
            )));
        }
        Ok(coll)
    }

    pub async fn find(&self, caller: &Caller, collection: &str, q: &Query) -> Result<Page> {
        let coll = self.coll(collection)?;
        if let Some(include) = &q.include {
            include::check(&coll, include, q.select.as_deref())?;
        }
        let mut conn = self.store.pool().acquire().await?;
        let mut page = query::find(&mut conn, &coll, caller, q).await?;
        if let Some(include) = &q.include {
            page.included = Some(
                include::fetch(
                    &self.store.collections(),
                    &mut conn,
                    caller,
                    &page.rows,
                    include,
                    q.spaces.as_deref(),
                )
                .await?,
            );
        }
        Ok(page)
    }

    /// The plan Postgres makes for a `nearest` query, with planner `settings` (`("enable_seqscan", "off")`)
    /// applied to the transaction it is planned in. For tests that check an index is used.
    #[doc(hidden)]
    pub async fn explain_nearest(
        &self,
        caller: &Caller,
        collection: &str,
        q: &Query,
        settings: &[(&str, &str)],
    ) -> Result<Vec<String>> {
        let coll = self.coll(collection)?;
        let mut conn = self.store.pool().acquire().await?;
        nearest::explain(&mut conn, &coll, caller, q, settings).await
    }

    /// Every matching row, a page at a time in `id` order: pass each page's `next` as `after` until it is `None`.
    /// A pass is stable while rows are written (see `bulk.rs`); `limit` is the page size (at most 1,000).
    pub async fn scan(&self, caller: &Caller, collection: &str, q: &Query) -> Result<Page> {
        let q = bulk::scan_query(q)?;
        self.find(caller, collection, &q).await
    }

    /// Writes up to 500 rows in one transaction, with the rows that failed reported by index.
    pub async fn import(
        &self,
        caller: &Caller,
        collection: &str,
        rows: &[Value],
        options: &ImportOptions,
    ) -> Result<ImportOutcome> {
        let coll = self.writable(collection)?;
        bulk::check_options(&coll, rows.len(), options)?;
        let idem = crate::idempotency::take();
        if let Some(key) = &idem
            && let Some(done) = bulk::replay(self.store, key).await?
        {
            return Ok(done);
        }
        if coll.is_log() {
            return log::import(self.store, &coll, caller, rows, idem.as_ref()).await;
        }
        bulk::import(self.store, &coll, caller, rows, options, idem.as_ref()).await
    }

    /// A log collection's rows after a `seq`, oldest first, with the `seq` to continue from.
    pub async fn tail(&self, caller: &Caller, collection: &str, q: &TailQuery) -> Result<Tail> {
        let coll = self.coll(collection)?;
        if !coll.is_log() {
            return Err(Error::invalid(format!(
                "{collection} is not a log collection: tail follows logs, a regular collection has the change feed"
            )));
        }
        log::tail(self.store, &coll, caller, q).await
    }

    /// Grouped counts, sums and extremes over the rows the caller may read.
    pub async fn aggregate(
        &self,
        caller: &Caller,
        collection: &str,
        q: &AggregateQuery,
    ) -> Result<Vec<Value>> {
        let coll = self.coll(collection)?;
        let mut conn = self.store.pool().acquire().await?;
        aggregate::aggregate(&mut conn, &coll, caller, q).await
    }

    /// Full-text search, best match first.
    pub async fn search(
        &self,
        caller: &Caller,
        collection: &str,
        text: &str,
        filter: Option<Value>,
        limit: i64,
    ) -> Result<Page> {
        let q = Query {
            filter,
            search: Some(text.to_string()),
            limit: Some(limit),
            ..Default::default()
        };
        self.find(caller, collection, &q).await
    }

    pub async fn get(&self, caller: &Caller, collection: &str, id: Uuid) -> Result<Option<Value>> {
        let coll = self.coll(collection)?;
        let mut conn = self.store.pool().acquire().await?;
        query::get(&mut conn, &coll, caller, id, false).await
    }

    /// A row in the trash.
    pub async fn get_trashed(
        &self,
        caller: &Caller,
        collection: &str,
        id: Uuid,
    ) -> Result<Option<Value>> {
        let coll = self.coll(collection)?;
        let mut conn = self.store.pool().acquire().await?;
        query::get(&mut conn, &coll, caller, id, true).await
    }

    /// The number of rows matching `where` and `search` (other fields of the query are ignored).
    pub async fn count(&self, caller: &Caller, collection: &str, q: &Query) -> Result<i64> {
        let coll = self.coll(collection)?;
        let mut conn = self.store.pool().acquire().await?;
        query::count(&mut conn, &coll, caller, q).await
    }

    /// Parses a filter for evaluating rows in memory (live queries).
    pub fn filter(&self, collection: &str, filter: &Value) -> Result<Filter> {
        Filter::parse(&*self.coll(collection)?, filter)
    }

    pub async fn insert(
        &self,
        caller: &Caller,
        collection: &str,
        values: Value,
    ) -> Result<WriteOutcome> {
        let values = as_object(values)?;
        let coll = self.writable(collection)?;
        let idem = crate::idempotency::take();
        if let Some(done) = self.replay(idem.as_ref()).await? {
            return Ok(done);
        }
        if coll.is_log() {
            let row = log::insert(self.store, &coll, caller, &values, idem.as_ref()).await?;
            return Ok(WriteOutcome {
                row: Some(row),
                change: None,
                mentions: Vec::new(),
            });
        }
        let mut w = WriteTx::begin(self.store).await?;
        let row = write::insert(&mut w, &coll, caller, &values).await?;
        self.finish(w, Some(row), idem).await
    }

    /// Updates fields of a row. With `rev`, fails with a conflict unless the row is still at
    /// that revision. A patch that changes nothing writes nothing.
    pub async fn update(
        &self,
        caller: &Caller,
        collection: &str,
        id: Uuid,
        patch: Value,
        rev: Option<i64>,
    ) -> Result<WriteOutcome> {
        let patch = as_object(patch)?;
        let coll = self.regular(collection)?;
        let idem = crate::idempotency::take();
        if let Some(done) = self.replay(idem.as_ref()).await? {
            return Ok(done);
        }
        let mut w = WriteTx::begin(self.store).await?;
        let row = write::update(&mut w, &coll, caller, id, &patch, rev).await?;
        self.finish(w, Some(row), idem).await
    }

    /// Deletes a row: soft when the collection has a trash.
    pub async fn delete(
        &self,
        caller: &Caller,
        collection: &str,
        id: Uuid,
    ) -> Result<WriteOutcome> {
        let coll = self.regular(collection)?;
        let idem = crate::idempotency::take();
        if let Some(done) = self.replay(idem.as_ref()).await? {
            return Ok(done);
        }
        let mut w = WriteTx::begin(self.store).await?;
        let row = write::delete(&mut w, &coll, caller, id).await?;
        self.finish(w, row, idem).await
    }

    pub async fn restore(
        &self,
        caller: &Caller,
        collection: &str,
        id: Uuid,
    ) -> Result<WriteOutcome> {
        let coll = self.regular(collection)?;
        let idem = crate::idempotency::take();
        if let Some(done) = self.replay(idem.as_ref()).await? {
            return Ok(done);
        }
        let mut w = WriteTx::begin(self.store).await?;
        let row = write::restore(&mut w, &coll, caller, id).await?;
        self.finish(w, Some(row), idem).await
    }

    /// Deletes a trashed row for good. Needs `manage` on it.
    pub async fn purge(&self, caller: &Caller, collection: &str, id: Uuid) -> Result<WriteOutcome> {
        let coll = self.regular(collection)?;
        let idem = crate::idempotency::take();
        if let Some(done) = self.replay(idem.as_ref()).await? {
            return Ok(done);
        }
        let mut w = WriteTx::begin(self.store).await?;
        write::purge(&mut w, &coll, caller, id).await?;
        self.finish(w, None, idem).await
    }

    pub async fn update_ops(
        &self,
        caller: &Caller,
        collection: &str,
        id: Uuid,
        patch: Value,
        rev: Option<i64>,
        ops: &operations::FieldOps,
    ) -> Result<WriteOutcome> {
        let coll = self.regular(collection)?;
        let idem = crate::idempotency::take();
        if let Some(done) = self.replay(idem.as_ref()).await? {
            return Ok(done);
        }
        let mut w = WriteTx::begin(self.store).await?;
        let row =
            write::update_ops(&mut w, &coll, caller, id, &as_object(patch)?, rev, ops).await?;
        self.finish(w, Some(row), idem).await
    }

    pub async fn upsert(
        &self,
        caller: &Caller,
        collection: &str,
        values: Value,
        on: &[String],
    ) -> Result<WriteOutcome> {
        let coll = self.regular(collection)?;
        let idem = crate::idempotency::take();
        if let Some(done) = self.replay(idem.as_ref()).await? {
            return Ok(done);
        }
        let mut w = WriteTx::begin(self.store).await?;
        let row = write::upsert(&mut w, &coll, caller, &as_object(values)?, on).await?;
        self.finish(w, Some(row), idem).await
    }

    /// The recorded outcome of a repeated idempotent write: its row, and no change or mention, since
    /// nothing is written again.
    async fn replay(&self, idem: Option<&crate::idempotency::Key>) -> Result<Option<WriteOutcome>> {
        Ok(self.replay_rows(idem).await?.map(|rows| WriteOutcome {
            row: rows.into_iter().next().flatten(),
            change: None,
            mentions: Vec::new(),
        }))
    }

    async fn replay_rows(
        &self,
        idem: Option<&crate::idempotency::Key>,
    ) -> Result<Option<Vec<Option<Value>>>> {
        let Some(key) = idem else { return Ok(None) };
        let mut conn = self.store.pool().acquire().await?;
        Ok(crate::idempotency::stored(&mut conn, key)
            .await?
            .map(|r| crate::idempotency::rows_of(&r)))
    }

    /// The recorded outcome of a repeated idempotent batch: its rows and the jobs it enqueued (read again, so that
    /// whoever tells the kernel about a new job can do what a crash between the commit and that left undone).
    async fn replay_batch(
        &self,
        idem: Option<&crate::idempotency::Key>,
    ) -> Result<Option<BatchOutcome>> {
        let Some(key) = idem else { return Ok(None) };
        let mut conn = self.store.pool().acquire().await?;
        let Some(done) = crate::idempotency::stored(&mut conn, key).await? else {
            return Ok(None);
        };
        drop(conn);
        let mut jobs = Vec::new();
        for id in crate::idempotency::jobs_of(&done) {
            if let Some(job) = self.store.jobs().get(id).await? {
                jobs.push(job);
            }
        }
        Ok(Some(BatchOutcome {
            rows: crate::idempotency::rows_of(&done),
            changes: Vec::new(),
            mentions: Vec::new(),
            jobs,
            repeat: true,
        }))
    }

    /// Records an idempotent write's rows in its transaction. `false` when a concurrent first attempt
    /// recorded the key meanwhile; the transaction is then rolled back.
    async fn record(
        w: &mut WriteTx,
        idem: Option<&crate::idempotency::Key>,
        rows: &[Option<Value>],
    ) -> Result<bool> {
        match idem {
            Some(key) => {
                crate::idempotency::record(&mut w.tx, key, &crate::idempotency::rows_response(rows))
                    .await
            }
            None => Ok(true),
        }
    }

    async fn finish(
        &self,
        mut w: WriteTx,
        row: Option<Value>,
        idem: Option<crate::idempotency::Key>,
    ) -> Result<WriteOutcome> {
        if !Self::record(&mut w, idem.as_ref(), std::slice::from_ref(&row)).await? {
            drop(w);
            return self.replay(idem.as_ref()).await?.ok_or_else(|| {
                Error::conflict("a write with this idempotency key is in progress")
            });
        }
        let (changes, mentions) = w.commit(self.store).await?;
        Ok(WriteOutcome {
            row,
            change: changes.into_iter().next(),
            mentions,
        })
    }

    /// Runs the operations in one transaction: all of them happen or none does.
    pub async fn batch(&self, caller: &Caller, ops: &[BatchOp]) -> Result<BatchOutcome> {
        self.batch_if(caller, ops, None, &caller.actor()).await
    }

    pub async fn batch_if(
        &self,
        caller: &Caller,
        ops: &[BatchOp],
        condition: Option<&conditions::BatchCondition>,
        holder: &str,
    ) -> Result<BatchOutcome> {
        if ops.len() > 1000 {
            return Err(Error::invalid("at most 1000 operations in a batch"));
        }
        let idem = crate::idempotency::take();
        if let Some(done) = self.replay_batch(idem.as_ref()).await? {
            return Ok(done);
        }
        let mut w = WriteTx::begin(self.store).await?;
        w.fence = condition.map(|c| (self.plugin.to_string(), c.clone(), holder.to_string()));
        let mut rows = Vec::with_capacity(ops.len());
        let mut jobs = Vec::new();
        let mut publishes: Vec<(&String, &Value)> = Vec::new();
        check_condition(&mut w.tx, self.plugin, condition, holder).await?;
        for (index, op) in ops.iter().enumerate() {
            let result: Result<Option<Value>> = async {
                Ok(match op {
                    BatchOp::Insert { collection, values } => Some(
                        write::insert(&mut w, &*self.regular(collection)?, caller, values).await?,
                    ),
                    BatchOp::Upsert {
                        collection,
                        values,
                        on,
                    } => Some(
                        write::upsert(&mut w, &*self.regular(collection)?, caller, values, on)
                            .await?,
                    ),
                    BatchOp::Update {
                        collection,
                        id,
                        patch,
                        rev,
                        ops,
                    } => Some(
                        write::update_ops(
                            &mut w,
                            &*self.regular(collection)?,
                            caller,
                            *id,
                            patch,
                            *rev,
                            ops,
                        )
                        .await?,
                    ),
                    BatchOp::Delete { collection, id } => {
                        write::delete(&mut w, &*self.regular(collection)?, caller, *id).await?
                    }
                    BatchOp::Restore { collection, id } => Some(
                        write::restore(&mut w, &*self.regular(collection)?, caller, *id).await?,
                    ),
                    BatchOp::Publish { topic, payload } => {
                        if !topic.starts_with(&format!("{}/", self.plugin)) {
                            return Err(Error::forbidden(
                                "a batch only publishes its plugin's topics",
                            ));
                        }
                        if serde_json::to_vec(payload)
                            .map_err(|e| Error::invalid(e.to_string()))?
                            .len()
                            > MAX_TOPIC_PAYLOAD
                        {
                            return Err(Error::invalid("a message of a topic is at most 256 KiB"));
                        }
                        // Written after the other steps: the topic's lock is held until commit, so the later it is
                        // taken the less it holds other publishers of the topic back.
                        publishes.push((topic, payload));
                        None
                    }
                    BatchOp::Enqueue { job } => {
                        if !job.kind.starts_with(&format!("{}/", self.plugin)) {
                            return Err(Error::forbidden(
                                "a batch only enqueues its plugin's jobs",
                            ));
                        }
                        let new = crate::jobs::NewJob {
                            kind: &job.kind,
                            params: &job.params,
                            max_attempts: job.max_attempts,
                            run_after: None,
                            subject: job.subject.as_deref(),
                            enqueued_by: &caller.actor(),
                        };
                        jobs.push(
                            crate::jobs::enqueue_on(&mut w.tx, &new, self.store.fence()).await?,
                        );
                        None
                    }
                })
            }
            .await;
            rows.push(result.map_err(|e| batch_error(e, index))?);
        }
        for (topic, payload) in publishes {
            crate::topics::publish_on(&mut w.tx, topic, payload, &caller.actor()).await?;
        }
        // Check expiry again immediately before commit while the grant is still locked.
        check_condition(&mut w.tx, self.plugin, condition, holder).await?;
        if let Some(key) = idem.as_ref() {
            let ids: Vec<Uuid> = jobs.iter().map(|j| j.id).collect();
            if !crate::idempotency::record(
                &mut w.tx,
                key,
                &crate::idempotency::batch_response(&rows, &ids),
            )
            .await?
            {
                drop(w);
                return self.replay_batch(idem.as_ref()).await?.ok_or_else(|| {
                    Error::conflict("a write with this idempotency key is in progress")
                });
            }
        }
        let (changes, mentions) = w.commit(self.store).await?;
        Ok(BatchOutcome {
            rows,
            changes,
            mentions,
            jobs,
            repeat: false,
        })
    }

    /// The field-level history of a row, newest first, if the caller may read the row.
    pub async fn history(
        &self,
        caller: &Caller,
        collection: &str,
        id: Uuid,
        limit: i64,
    ) -> Result<Vec<crate::history::HistoryEntry>> {
        let coll = self.coll(collection)?;
        if coll.is_log() {
            return Err(Error::invalid(format!(
                "{collection} is a log collection: it keeps no history"
            )));
        }
        let mut conn = self.store.pool().acquire().await?;
        let visible = query::get(&mut conn, &coll, caller, id, false)
            .await?
            .is_some()
            || (coll.def.trash
                && query::get(&mut conn, &coll, caller, id, true)
                    .await?
                    .is_some());
        if !visible {
            return Err(Error::not_found(format!("no {collection} row {id}")));
        }
        crate::history::for_row(&mut conn, self.plugin, collection, id, limit).await
    }
}

fn as_object(v: Value) -> Result<Map<String, Value>> {
    match v {
        Value::Object(m) => Ok(m),
        _ => Err(Error::invalid("expected an object")),
    }
}

fn batch_error(e: Error, index: usize) -> Error {
    if e.code() != rookery_proto::ErrorCode::Conflict {
        return e;
    }
    let mut details = e.details().unwrap_or_else(|| serde_json::json!({}));
    details["index"] = serde_json::json!(index);
    Error::ConflictWith {
        message: e.to_string(),
        details,
    }
}

pub(crate) async fn check_condition(
    conn: &mut PgConnection,
    plugin: &str,
    condition: Option<&conditions::BatchCondition>,
    holder: &str,
) -> Result<()> {
    let Some(condition) = condition else {
        return Ok(());
    };
    // The lease of the plugin's name, held now by the caller (or one of its workers, `<holder>/<worker>`),
    // at the fence the batch names: the fence tells this hand from an earlier one of the same holder.
    let held: Option<bool> = sqlx::query_scalar(
        "SELECT expires_at > clock_timestamp() AND (holder = $3 OR starts_with(holder, $3 || '/'))
         FROM rookery.leases WHERE name = $1 AND fence = $2 FOR UPDATE",
    )
    .bind(format!("{plugin}/{}", condition.lease.key))
    .bind(condition.lease.token)
    .bind(holder)
    .fetch_optional(&mut *conn)
    .await?;
    if held != Some(true) {
        return Err(Error::ConflictWith {
            message: "batch lease condition failed".into(),
            details: serde_json::json!({"index": 0, "condition": "lease"}),
        });
    }
    Ok(())
}
