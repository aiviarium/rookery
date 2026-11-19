//! Numbered plugin migrations: the changes `apply` refuses (rename, drop, change type, ...)
//! and data transforms that run as host code.
//!
//! A plugin's `migrations/` folder holds one JSON file per migration; the kernel parses them
//! into [`PluginMigration`]s, for example `0002.json`:
//!
//! ```json
//! { "steps": [
//!     { "op": "renameField", "collection": "issues", "from": "owner", "to": "assignee" },
//!     { "op": "transform", "name": "fillAssignee" }
//! ] }
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use super::schema::{CollectionDef, FieldDef, FieldType, RESERVED_NAMES, valid_ident};
use super::values::row_out;
use super::write::{self, WriteTx};
use super::{Collection, ddl, load_stored, lock_plugin, reindex, save_stored};
use crate::Store;
use crate::caller::Caller;
use crate::error::{Error, Result};
use crate::util::quote_ident;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMigration {
    /// The migration's number; migrations run in order, each once.
    pub version: i64,
    /// Also run when the plugin is installed for the first time, where a migration is skipped because the
    /// manifest already describes its result: for one that adopts data that exists outside the plugin's
    /// collections (`MigrationTx::legacy`).
    #[serde(default)]
    pub on_install: bool,
    pub steps: Vec<MigrationStep>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum MigrationStep {
    RenameField {
        collection: String,
        from: String,
        to: String,
    },
    DropField {
        collection: String,
        field: String,
    },
    /// Replaces a field's definition with one of another type, converting the column with a
    /// cast. Conversions a cast cannot do need a transform.
    ChangeType {
        collection: String,
        field: Box<FieldDef>,
    },
    RenameCollection {
        from: String,
        to: String,
    },
    DropCollection {
        collection: String,
    },
    /// Deletes the rows in the trash for good, so that trash can be turned off.
    PurgeTrash {
        collection: String,
    },
    /// Runs host code through the [`Transformer`].
    Transform {
        name: String,
    },
}

/// Runs data-transform steps. The kernel implements it by calling the plugin's host half.
#[async_trait::async_trait]
pub trait Transformer: Send + Sync {
    async fn transform(&self, name: &str, tx: &mut MigrationTx<'_>) -> Result<()>;
}

/// A transformer for plugins without transform steps: any transform fails.
pub struct NoTransforms;

#[async_trait::async_trait]
impl Transformer for NoTransforms {
    async fn transform(&self, name: &str, _tx: &mut MigrationTx<'_>) -> Result<()> {
        Err(Error::invalid(format!("no transformer for step {name}")))
    }
}

/// A migration in progress: its transaction and the collection definitions as the earlier
/// steps left them. Writes go through the normal write path (entities, references, history,
/// change log) as the system, without access checks.
pub struct MigrationTx<'a> {
    store: &'a Store,
    plugin: String,
    w: WriteTx,
    defs: Vec<CollectionDef>,
}

impl<'a> MigrationTx<'a> {
    pub fn plugin(&self) -> &str {
        &self.plugin
    }

    /// The store, for reads outside the migration's transaction.
    pub fn store(&self) -> &Store {
        self.store
    }

    /// The collection definitions as they are at this step.
    pub fn collections(&self) -> &[CollectionDef] {
        &self.defs
    }

    fn coll(&self, name: &str) -> Result<Collection> {
        self.defs
            .iter()
            .find(|d| d.name == name)
            .map(|d| Collection::new(&self.plugin, d.clone()))
            .ok_or_else(|| Error::not_found(format!("no collection {name}")))
    }

    /// Rows in id order after `after`, in every scope and including the trash.
    pub async fn rows(
        &mut self,
        collection: &str,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<Value>> {
        let coll = self.coll(collection)?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT to_jsonb(t) - '_search' AS row FROM {} t WHERE id > $1 ORDER BY id LIMIT $2",
            coll.table
        )))
        .bind(after.unwrap_or(Uuid::nil()))
        .bind(limit.clamp(1, 10_000))
        .fetch_all(&mut *self.w.tx)
        .await?;
        rows.into_iter()
            .map(|r| Ok(row_out(&coll, r.try_get("row")?)))
            .collect()
    }

    /// Rows of a table a release of Rookery retired, parked in the `rookery_legacy` schema, as JSON in a stable
    /// order after the cursor of the page before (`None` for the first page); the cursor to go on from is
    /// the second value (`None` when there are no more). A table that is not there has no rows.
    pub async fn legacy(
        &mut self,
        table: &str,
        after: Option<&str>,
        limit: i64,
    ) -> Result<(Vec<Value>, Option<String>)> {
        if table.is_empty()
            || !table.starts_with(|c: char| c.is_ascii_lowercase())
            || !table
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(Error::invalid(format!("{table:?} is not a table name")));
        }
        let exists: bool = sqlx::query_scalar(
            "SELECT to_regclass(format('rookery_legacy.%I', $1::text)) IS NOT NULL",
        )
        .bind(table)
        .fetch_one(&mut *self.w.tx)
        .await?;
        if !exists {
            return Ok((Vec::new(), None));
        }
        let limit = limit.clamp(1, 10_000);
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT ctid::text AS cursor, to_jsonb(t) AS row FROM rookery_legacy.{} t \
             WHERE ($1::text IS NULL OR ctid > $1::tid) ORDER BY ctid LIMIT $2",
            quote_ident(table)
        )))
        .bind(after)
        .bind(limit)
        .fetch_all(&mut *self.w.tx)
        .await?;
        let next = rows
            .last()
            .map(|r| r.try_get::<String, _>("cursor"))
            .transpose()?;
        let rows = rows
            .into_iter()
            .map(|r| r.try_get::<Value, _>("row"))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok((rows, next))
    }

    fn system(space: Option<Uuid>, user: Option<Uuid>) -> Caller {
        Caller {
            user,
            principal: "system:rookery".into(),
            space,
            admin: true,
            admin_of: None,
        }
    }

    async fn scope_of(
        &mut self,
        coll: &Collection,
        id: Uuid,
    ) -> Result<(Option<Uuid>, Option<Uuid>)> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT space_id, user_id FROM {} WHERE id = $1",
            coll.table
        )))
        .bind(id)
        .fetch_optional(&mut *self.w.tx)
        .await?
        .ok_or_else(|| Error::not_found(format!("no {} row {id}", coll.def.name)))?;
        Ok((r.try_get("space_id")?, r.try_get("user_id")?))
    }

    pub async fn update(&mut self, collection: &str, id: Uuid, patch: Value) -> Result<Value> {
        let coll = self.coll(collection)?;
        let (space, user) = self.scope_of(&coll, id).await?;
        let patch = match patch {
            Value::Object(m) => m,
            _ => return Err(Error::invalid("expected an object")),
        };
        write::update(
            &mut self.w,
            &coll,
            &Self::system(space, user),
            id,
            &patch,
            None,
        )
        .await
    }

    pub async fn insert(
        &mut self,
        collection: &str,
        values: Value,
        space: Option<Uuid>,
        user: Option<Uuid>,
    ) -> Result<Value> {
        let coll = self.coll(collection)?;
        let values = match values {
            Value::Object(m) => m,
            _ => return Err(Error::invalid("expected an object")),
        };
        write::insert(&mut self.w, &coll, &Self::system(space, user), &values).await
    }

    pub async fn delete(&mut self, collection: &str, id: Uuid) -> Result<()> {
        let coll = self.coll(collection)?;
        let (space, user) = self.scope_of(&coll, id).await?;
        write::delete(&mut self.w, &coll, &Self::system(space, user), id, None)
            .await
            .map(|_| ())
    }
}

/// The fields a moved row gets besides its own, from the row itself.
pub type ExtraFields<'a> =
    dyn Fn(&serde_json::Map<String, Value>) -> serde_json::Map<String, Value> + Sync + 'a;

/// Moves the rows of a user-scope collection into a space-scope one, for a release that changes where a
/// kind of row lives. Each row keeps its id and goes into its owner's personal space, with the fields it has
/// that `into` declares and what `extra` adds; the old row is deleted. Writes go through the normal write
/// path as the system, so entities, references and the change log follow. Returns how many rows moved, which
/// is zero once they have, so it is safe to call every time a plugin loads.
pub(crate) async fn move_user_rows(
    store: &Store,
    plugin: &str,
    from: &str,
    into: &str,
    extra: &ExtraFields<'_>,
) -> Result<usize> {
    super::validate_plugin_id(plugin)?;
    let mut moved = 0;
    let mut after = None;
    loop {
        let mut w = WriteTx::begin(store).await?;
        lock_plugin(&mut w.tx, plugin).await?;
        let defs = load_stored(&mut w.tx, plugin).await?;
        if !defs.iter().any(|d| d.name == from) || !defs.iter().any(|d| d.name == into) {
            return Ok(moved);
        }
        let mut mtx = MigrationTx {
            store,
            plugin: plugin.to_string(),
            w,
            defs,
        };
        let page = mtx.rows(from, after, 200).await?;
        let Some(last) = page
            .last()
            .and_then(|r| r["id"].as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            return Ok(moved);
        };
        after = Some(last);
        let fields: Vec<String> = mtx
            .defs
            .iter()
            .find(|d| d.name == into)
            .map(|d| d.fields.iter().map(|f| f.name.clone()).collect())
            .unwrap_or_default();
        for row in page {
            let Some(old) = row["id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) else {
                continue;
            };
            let coll = mtx.coll(from)?;
            let (_, user) = mtx.scope_of(&coll, old).await?;
            let Some(user) = user else { continue };
            let space = store.spaces().personal(user).await?.id;
            let mut values = serde_json::Map::new();
            values.insert("id".into(), Value::String(old.to_string()));
            if let Some(r) = row.as_object() {
                for (k, v) in r {
                    if k != "id" && fields.contains(k) && !v.is_null() {
                        values.insert(k.clone(), v.clone());
                    }
                }
                values.extend(extra(r));
            }
            mtx.delete(from, old).await?;
            mtx.insert(into, Value::Object(values), Some(space), Some(user))
                .await?;
            moved += 1;
        }
        let MigrationTx { w, .. } = mtx;
        w.commit(store).await?;
    }
}

pub(crate) async fn run(
    store: &Store,
    plugin: &str,
    migrations: &[PluginMigration],
    transformer: &dyn Transformer,
) -> Result<Vec<i64>> {
    super::validate_plugin_id(plugin)?;
    let mut sorted: Vec<&PluginMigration> = migrations.iter().collect();
    sorted.sort_by_key(|m| m.version);
    if sorted.windows(2).any(|w| w[0].version == w[1].version) {
        return Err(Error::invalid("two migrations have the same number"));
    }
    let mut applied = Vec::new();
    for m in sorted {
        if m.version <= 0 {
            return Err(Error::invalid("migration numbers start at 1"));
        }
        let mut w = WriteTx::begin(store).await?;
        lock_plugin(&mut w.tx, plugin).await?;
        let defs = load_stored(&mut w.tx, plugin).await?;
        let current: i64 =
            sqlx::query_scalar("SELECT version FROM rookery.plugin_schemas WHERE plugin = $1")
                .bind(plugin)
                .fetch_optional(&mut *w.tx)
                .await?
                .unwrap_or(0);
        if m.version <= current {
            continue;
        }
        // A plugin with raw SQL has views over its tables, which block changes to their columns: they go
        // before the steps and come back from the new definitions in the same transaction.
        let views_of = if crate::sql::views::is_enabled(&mut w.tx, plugin).await? {
            let names = store.sql().names(plugin)?;
            crate::sql::views::drop_all(&mut w.tx, &names).await?;
            Some(names)
        } else {
            None
        };
        let mut mtx = MigrationTx {
            store,
            plugin: plugin.to_string(),
            w,
            defs,
        };
        for step in &m.steps {
            apply_step(&mut mtx, step, transformer)
                .await
                .map_err(|e| Error::invalid(format!("migration {} of {plugin}: {e}", m.version)))?;
        }
        for d in &mtx.defs {
            d.validate()?;
        }
        let MigrationTx {
            w: mut wtx, defs, ..
        } = mtx;
        save_stored(&mut wtx.tx, plugin, &defs).await?;
        if let Some(names) = &views_of {
            crate::sql::views::sync(&mut wtx.tx, names, &defs).await?;
        }
        sqlx::query("UPDATE rookery.plugin_schemas SET version = $2 WHERE plugin = $1")
            .bind(plugin)
            .bind(m.version)
            .execute(&mut *wtx.tx)
            .await?;
        sqlx::query("INSERT INTO rookery.plugin_migrations (plugin, version) VALUES ($1, $2)")
            .bind(plugin)
            .bind(m.version)
            .execute(&mut *wtx.tx)
            .await?;
        wtx.commit(store).await?;
        store.collections().reload().await?;
        applied.push(m.version);
    }
    Ok(applied)
}

fn def_mut<'d>(defs: &'d mut [CollectionDef], name: &str) -> Result<&'d mut CollectionDef> {
    defs.iter_mut()
        .find(|d| d.name == name)
        .ok_or_else(|| Error::not_found(format!("no collection {name}")))
}

/// Renames indexes whose derived names changed between two versions of a collection with the
/// same index structure.
async fn rename_indexes(
    mtx: &mut MigrationTx<'_>,
    old: &Collection,
    new: &Collection,
) -> Result<()> {
    let (a, b) = (ddl::index_specs(old), ddl::index_specs(new));
    for (x, y) in a.iter().zip(b.iter()) {
        if x.name != y.name {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER INDEX IF EXISTS {}.{} RENAME TO {}",
                quote_ident(&old.schema),
                quote_ident(&x.name),
                quote_ident(&y.name)
            )))
            .execute(&mut *mtx.w.tx)
            .await?;
        }
    }
    Ok(())
}

/// Drops indexes of `old` that `new` does not have and creates those `new` adds.
async fn sync_indexes(mtx: &mut MigrationTx<'_>, old: &Collection, new: &Collection) -> Result<()> {
    let (a, b) = (ddl::index_specs(old), ddl::index_specs(new));
    for x in a.iter().filter(|x| !b.iter().any(|y| y.name == x.name)) {
        sqlx::query(sqlx::AssertSqlSafe(
            ddl::drop_index_sql(old, &x.name).to_string(),
        ))
        .execute(&mut *mtx.w.tx)
        .await?;
    }
    for y in b.iter().filter(|y| !a.iter().any(|x| x.name == y.name)) {
        sqlx::query(sqlx::AssertSqlSafe(y.create.to_string()))
            .execute(&mut *mtx.w.tx)
            .await?;
    }
    Ok(())
}

fn check_new_field_name(def: &CollectionDef, name: &str) -> Result<()> {
    if !valid_ident(name, 63) || RESERVED_NAMES.contains(&name) {
        return Err(Error::invalid(format!(
            "{name:?} is not a valid field name"
        )));
    }
    if def.field(name).is_some() {
        return Err(Error::invalid(format!("field {name} exists")));
    }
    Ok(())
}

fn rename_in(list: &mut [Vec<String>], from: &str, to: &str) {
    for cols in list {
        for c in cols.iter_mut() {
            if c == from {
                *c = to.to_string();
            }
        }
    }
}

fn using_expr(from: &FieldType, to: &FieldType, col: &str) -> Result<String> {
    use FieldType as T;
    let texty = |t: &T| {
        matches!(
            t,
            T::Text | T::Longtext | T::Path | T::Enum { .. } | T::Ref { .. }
        )
    };
    let special = |t: &T| matches!(t, T::Bytes { .. } | T::Blob | T::Vector { .. });
    if special(from) || special(to) {
        if from != to {
            return Err(Error::invalid(
                "bytes, blob and vector type changes need a new field and a transform",
            ));
        }
        return Ok(col.to_string());
    }
    Ok(match (from, to) {
        (T::Json | T::Richtext, t) if texty(t) => format!("{col} #>> '{{}}'"),
        (T::Refs { .. }, t) if texty(t) => format!("array_to_string({col}, ',')"),
        (_, t) if texty(t) => format!("{col}::text"),
        (f, T::Refs { .. }) if texty(f) => {
            format!("CASE WHEN {col} IS NULL THEN NULL ELSE ARRAY[{col}] END")
        }
        (T::Richtext, T::Json) | (T::Json, T::Richtext) => col.to_string(),
        (_, T::Json) => format!("to_jsonb({col})"),
        (_, T::Richtext) => return Err(Error::invalid("converting to richtext needs a transform")),
        (_, t) => format!("{col}::{}", t.sql_type()),
    })
}

async fn apply_step(
    mtx: &mut MigrationTx<'_>,
    step: &MigrationStep,
    transformer: &dyn Transformer,
) -> Result<()> {
    let plugin = mtx.plugin.clone();
    match step {
        MigrationStep::RenameField {
            collection,
            from,
            to,
        } => {
            let old = mtx.coll(collection)?;
            let def = def_mut(&mut mtx.defs, collection)?;
            def.field(from)
                .ok_or_else(|| Error::not_found(format!("no field {from}")))?;
            check_new_field_name(def, to)?;
            for f in def.fields.iter_mut().filter(|f| &f.name == from) {
                f.name = to.clone();
            }
            rename_in(&mut def.indexes, from, to);
            rename_in(&mut def.unique, from, to);
            if let Some(e) = def.entity.as_mut() {
                for slot in [&mut e.subtitle, &mut e.icon, &mut e.parent, &mut e.private] {
                    if slot.as_deref() == Some(from.as_str()) {
                        *slot = Some(to.clone());
                    }
                }
                if &e.title == from {
                    e.title = to.clone();
                }
            }
            let new = mtx.coll(collection)?;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER TABLE {} RENAME COLUMN {} TO {}",
                old.table,
                quote_ident(from),
                quote_ident(to)
            )))
            .execute(&mut *mtx.w.tx)
            .await?;
            rename_indexes(mtx, &old, &new).await?;
            for table in ["rookery.refs", "rookery.sequences"] {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE {table} SET field = $4 WHERE plugin = $1 AND collection = $2 AND field = $3"
                )))
                .bind(&plugin)
                .bind(collection)
                .bind(from)
                .bind(to)
                .execute(&mut *mtx.w.tx)
                .await?;
            }
        }
        MigrationStep::DropField { collection, field } => {
            let old = mtx.coll(collection)?;
            let def = def_mut(&mut mtx.defs, collection)?;
            let f = def
                .field(field)
                .cloned()
                .ok_or_else(|| Error::not_found(format!("no field {field}")))?;
            if let Some(e) = &def.entity {
                let used = e.title == *field
                    || [&e.subtitle, &e.icon, &e.parent, &e.private]
                        .iter()
                        .any(|s| s.as_deref() == Some(field.as_str()));
                if used {
                    return Err(Error::invalid(format!(
                        "field {field} is used by the entity options"
                    )));
                }
            }
            def.fields.retain(|x| &x.name != field);
            def.indexes.retain(|cols| !cols.contains(field));
            def.unique.retain(|cols| !cols.contains(field));
            let new = mtx.coll(collection)?;
            sync_indexes(mtx, &old, &new).await?;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER TABLE {} DROP COLUMN {}",
                old.table,
                quote_ident(field)
            )))
            .execute(&mut *mtx.w.tx)
            .await?;
            for table in ["rookery.refs", "rookery.sequences"] {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "DELETE FROM {table} WHERE plugin = $1 AND collection = $2 AND field = $3"
                )))
                .bind(&plugin)
                .bind(collection)
                .bind(field)
                .execute(&mut *mtx.w.tx)
                .await?;
            }
            if f.search {
                reindex(&mut mtx.w.tx, &new, true, new.def.entity.is_some(), false).await?;
            }
        }
        MigrationStep::ChangeType { collection, field } => {
            let old = mtx.coll(collection)?;
            let prev = old
                .def
                .field(&field.name)
                .cloned()
                .ok_or_else(|| Error::not_found(format!("no field {}", field.name)))?;
            if matches!(field.ty, FieldType::Sequence { .. })
                || matches!(prev.ty, FieldType::Sequence { .. })
            {
                return Err(Error::invalid("sequence fields cannot change type"));
            }
            let def = def_mut(&mut mtx.defs, collection)?;
            for f in def.fields.iter_mut().filter(|f| f.name == field.name) {
                *f = field.as_ref().clone();
            }
            def.validate()?;
            let new = mtx.coll(collection)?;
            let col = quote_ident(&field.name);
            // Drop the old indexes first: the new type may not support them.
            for x in ddl::index_specs(&old) {
                sqlx::query(sqlx::AssertSqlSafe(
                    ddl::drop_index_sql(&old, &x.name).to_string(),
                ))
                .execute(&mut *mtx.w.tx)
                .await?;
            }
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER TABLE {} ALTER COLUMN {col} TYPE {} USING {}",
                old.table,
                field.ty.sql_type(),
                using_expr(&prev.ty, &field.ty, &col)?
            )))
            .execute(&mut *mtx.w.tx)
            .await?;
            check_values(mtx, &new, field).await?;
            for y in ddl::index_specs(&new) {
                sqlx::query(sqlx::AssertSqlSafe(y.create.to_string()))
                    .execute(&mut *mtx.w.tx)
                    .await?;
            }
            sqlx::query(
                "DELETE FROM rookery.refs WHERE plugin = $1 AND collection = $2 AND field = $3",
            )
            .bind(&plugin)
            .bind(collection)
            .bind(&field.name)
            .execute(&mut *mtx.w.tx)
            .await?;
            reindex(&mut mtx.w.tx, &new, true, new.def.entity.is_some(), true).await?;
        }
        MigrationStep::RenameCollection { from, to } => {
            if !valid_ident(to, 48) {
                return Err(Error::invalid(format!(
                    "{to:?} is not a valid collection name"
                )));
            }
            if mtx.defs.iter().any(|d| &d.name == to) {
                return Err(Error::invalid(format!("collection {to} exists")));
            }
            let old = mtx.coll(from)?;
            if old.def.is_log() {
                return Err(Error::invalid(format!(
                    "log collection {from} cannot be renamed: its partitions and sequence carry its name"
                )));
            }
            def_mut(&mut mtx.defs, from)?.name = to.clone();
            let new = mtx.coll(to)?;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER TABLE {} RENAME TO {}",
                old.table,
                quote_ident(to)
            )))
            .execute(&mut *mtx.w.tx)
            .await?;
            for suffix in ["scope", "search"] {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "ALTER INDEX IF EXISTS {}.{} RENAME TO {}",
                    quote_ident(&old.schema),
                    quote_ident(&format!("{from}__{suffix}")),
                    quote_ident(&format!("{to}__{suffix}"))
                )))
                .execute(&mut *mtx.w.tx)
                .await?;
            }
            rename_indexes(mtx, &old, &new).await?;
            for table in [
                "rookery.refs",
                "rookery.sequences",
                "rookery.changes",
                "rookery.history",
                "rookery.entities",
            ] {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE {table} SET collection = $3 WHERE plugin = $1 AND collection = $2"
                )))
                .bind(&plugin)
                .bind(from)
                .bind(to)
                .execute(&mut *mtx.w.tx)
                .await?;
            }
            // References from rows that are not entities carry the collection in their id.
            sqlx::query(
                "UPDATE rookery.refs SET src_id = $3 || substr(src_id, length($2) + 1) \
                 WHERE src_scheme = 'row' AND src_id LIKE $2 || '%'",
            )
            .bind(&plugin)
            .bind(format!("{plugin}/{from}/"))
            .bind(format!("{plugin}/{to}/"))
            .execute(&mut *mtx.w.tx)
            .await?;
        }
        MigrationStep::DropCollection { collection } => {
            let old = mtx.coll(collection)?;
            sqlx::query(
                "UPDATE rookery.refs r SET dangling = true FROM rookery.entities e \
                 WHERE e.plugin = $1 AND e.collection = $2 AND r.dst_scheme = e.scheme AND r.dst_id = e.id",
            )
            .bind(&plugin)
            .bind(collection)
            .execute(&mut *mtx.w.tx)
            .await?;
            sqlx::query(
                "DELETE FROM rookery.grants g USING rookery.entities e \
                 WHERE e.plugin = $1 AND e.collection = $2 AND g.resource = e.uri",
            )
            .bind(&plugin)
            .bind(collection)
            .execute(&mut *mtx.w.tx)
            .await?;
            for table in ["rookery.entities", "rookery.refs", "rookery.sequences"] {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "DELETE FROM {table} WHERE plugin = $1 AND collection = $2"
                )))
                .bind(&plugin)
                .bind(collection)
                .execute(&mut *mtx.w.tx)
                .await?;
            }
            sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE {}", old.table)))
                .execute(&mut *mtx.w.tx)
                .await?;
            if old.def.is_log() {
                // The partitions went with the table; the sequence is the collection's own and goes too.
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "DROP SEQUENCE IF EXISTS {}",
                    super::ddl::log_sequence(&old)
                )))
                .execute(&mut *mtx.w.tx)
                .await?;
            }
            mtx.defs.retain(|d| &d.name != collection);
        }
        MigrationStep::PurgeTrash { collection } => {
            let coll = mtx.coll(collection)?;
            let ids: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {} WHERE deleted_at IS NOT NULL RETURNING id",
                coll.table
            )))
            .fetch_all(&mut *mtx.w.tx)
            .await?;
            for id in ids {
                match coll.entity_uri(id) {
                    Some(uri) => {
                        crate::entities::delete(&mut mtx.w.tx, &uri).await?;
                    }
                    None => {
                        crate::refs::delete_from(
                            &mut mtx.w.tx,
                            "row",
                            &format!("{plugin}/{collection}/{id}"),
                        )
                        .await?;
                    }
                }
            }
            def_mut(&mut mtx.defs, collection)?.trash = false;
        }
        MigrationStep::Transform { name } => transformer.transform(name, mtx).await?,
    }
    Ok(())
}

/// After a type change: enum values and references must be valid.
async fn check_values(
    mtx: &mut MigrationTx<'_>,
    coll: &Collection,
    field: &FieldDef,
) -> Result<()> {
    let col = quote_ident(&field.name);
    let bad: i64 = match &field.ty {
        FieldType::Enum { values } => {
            let allowed: Vec<&str> = values.iter().map(|v| v.value()).collect();
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT count(*) FROM {} WHERE {col} IS NOT NULL AND {col} <> ALL($1)",
                coll.table
            )))
            .bind(allowed)
            .fetch_one(&mut *mtx.w.tx)
            .await?
        }
        FieldType::Ref { to } => {
            let pattern = match to {
                Some(t) => format!("^{}://\\S+$", regex_escape(t)),
                None => "^[a-z][a-z0-9+.-]*://\\S+$".to_string(),
            };
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT count(*) FROM {} WHERE {col} IS NOT NULL AND {col} !~ $1",
                coll.table
            )))
            .bind(pattern)
            .fetch_one(&mut *mtx.w.tx)
            .await?
        }
        _ => 0,
    };
    if bad > 0 {
        return Err(Error::invalid(format!(
            "{bad} rows have values that are not valid for the new type of {}",
            field.name
        )));
    }
    Ok(())
}

fn regex_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() {
                vec![c]
            } else {
                vec!['\\', c]
            }
        })
        .collect()
}
