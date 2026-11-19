//! Writes. Everything a write implies happens in its transaction: sequences, the search
//! vector, the entity index, the reference index, history and the change log entry.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::{PgConnection, Postgres, QueryBuilder, Row, Transaction};
use uuid::Uuid;

use super::constraints;
use super::richtext;
use super::schema::{FieldDef, FieldType, Scope};
use super::values::{Bind, SqlBind, input, row_out};
use super::{Collection, container_uri};
use crate::Store;
use crate::caller::{Caller, Level};
use crate::entities::{CollectionRef, EntityRecord, MAX_SEARCH_TEXT};
use crate::error::{Error, Result};
use crate::feed::{Change, ChangeOp, PendingChange};
use crate::refs::RefSource;
use crate::uri::Uri;
use crate::util::{new_id, quote_ident, truncate_str};

/// A user mentioned for the first time by a write. The kernel notifies them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mention {
    pub user: Uuid,
    pub plugin: String,
    pub collection: String,
    pub row_id: Uuid,
    pub field: String,
    /// The row's entity URI, for entity collections.
    pub entity: Option<String>,
}

/// The result of a single write.
#[derive(Debug, Clone)]
pub struct WriteOutcome {
    /// The row after the write; `None` after a hard delete.
    pub row: Option<Value>,
    /// The change log entry; `None` when nothing changed.
    pub change: Option<Arc<Change>>,
    pub mentions: Vec<Mention>,
}

/// One operation of a batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum BatchOp {
    Insert {
        collection: String,
        values: Map<String, Value>,
    },
    Upsert {
        collection: String,
        values: Map<String, Value>,
        on: Vec<String>,
    },
    Publish {
        topic: String,
        payload: Value,
    },
    Enqueue {
        job: BatchJob,
    },
    Update {
        #[serde(default)]
        ops: super::operations::FieldOps,
        collection: String,
        id: Uuid,
        patch: Map<String, Value>,
        #[serde(default)]
        rev: Option<i64>,
    },
    Delete {
        collection: String,
        id: Uuid,
    },
    Restore {
        collection: String,
        id: Uuid,
    },
}

/// A job a batch enqueues when it commits: of a kind of the plugin's, `<plugin>/<name>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchJob {
    pub kind: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default = "one")]
    pub max_attempts: i32,
}

fn one() -> i32 {
    1
}

#[derive(Debug, Clone)]
pub struct BatchOutcome {
    /// The row after each operation, in order (`None` after a hard delete).
    pub rows: Vec<Option<Value>>,
    pub changes: Vec<Arc<Change>>,
    pub mentions: Vec<Mention>,
    /// The jobs the batch enqueued (those a repeat of it enqueued the first time, when it was one).
    pub jobs: Vec<crate::jobs::Job>,
    /// An idempotent batch that had committed already: nothing was written now.
    pub repeat: bool,
}

/// A write transaction: the database transaction plus what to log and report at commit.
pub(crate) struct WriteTx {
    pub tx: Transaction<'static, Postgres>,
    pub pending: Vec<PendingChange>,
    pub fence: Option<(String, super::conditions::BatchCondition, String)>,
    pub mentions: Vec<Mention>,
}

impl WriteTx {
    pub async fn begin(store: &Store) -> Result<WriteTx> {
        Ok(WriteTx {
            tx: store.pool().begin().await?,
            pending: Vec::new(),
            fence: None,
            mentions: Vec::new(),
        })
    }

    pub async fn commit(self, store: &Store) -> Result<(Vec<Arc<Change>>, Vec<Mention>)> {
        let changes = crate::feed::commit(store, self.tx, self.pending, self.fence).await?;
        Ok((changes, self.mentions))
    }
}

pub(crate) struct RowScope {
    pub space: Option<Uuid>,
    pub user: Option<Uuid>,
}

pub(crate) fn write_scope(coll: &Collection, caller: &Caller) -> Result<RowScope> {
    coll.def.access.check_admin(&coll.def.name, caller)?;
    Ok(match coll.def.scope {
        Scope::Global => RowScope {
            space: None,
            user: None,
        },
        Scope::Space => RowScope {
            space: Some(caller.require_space()?),
            user: None,
        },
        Scope::User => RowScope {
            space: None,
            user: Some(caller.require_user()?),
        },
    })
}

pub(crate) fn checks_access(coll: &Collection, caller: &Caller) -> bool {
    !caller.admin && coll.def.scope != Scope::User
}

/// The text of a field value: strings as they are, rich text as plain text.
fn text_of(f: &FieldDef, v: &Value) -> String {
    match (&f.ty, v) {
        (FieldType::Richtext, doc) => richtext::plain_text(doc),
        (_, Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Text for the search vector: weight A (text fields) and B (everything else).
fn search_texts(coll: &Collection, row: &Map<String, Value>) -> (String, String) {
    let (mut a, mut b) = (String::new(), String::new());
    for f in coll.def.fields.iter().filter(|f| f.search) {
        let Some(v) = row.get(&f.name) else { continue };
        let t = text_of(f, v);
        if t.is_empty() {
            continue;
        }
        let dst = if matches!(f.ty, FieldType::Text) {
            &mut a
        } else {
            &mut b
        };
        if !dst.is_empty() {
            dst.push(' ');
        }
        dst.push_str(&t);
    }
    (
        truncate_str(&a, MAX_SEARCH_TEXT).to_string(),
        truncate_str(&b, MAX_SEARCH_TEXT).to_string(),
    )
}

fn push_search_expr(qb: &mut QueryBuilder<Postgres>, (a, b): (String, String)) {
    qb.push("setweight(to_tsvector('simple', ");
    qb.push_bind(a);
    qb.push("), 'A') || setweight(to_tsvector('simple', ");
    qb.push_bind(b);
    qb.push("), 'B')");
}

/// The SQL that recomputes `_search` for an output row (used by reindexing).
pub(crate) async fn update_search(
    conn: &mut PgConnection,
    coll: &Collection,
    row: &Value,
) -> Result<()> {
    let Some(m) = row.as_object() else {
        return Ok(());
    };
    let id = row_id(row)?;
    let mut qb = QueryBuilder::<Postgres>::new(format!("UPDATE {} SET _search = ", coll.table));
    push_search_expr(&mut qb, search_texts(coll, m));
    qb.push(" WHERE id = ");
    qb.push_bind(id);
    qb.build().execute(conn).await?;
    Ok(())
}

pub(crate) fn row_id(row: &Value) -> Result<Uuid> {
    row.get("id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| Error::internal("row without id"))
}

fn opt_uuid(row: &Value, key: &str) -> Option<Uuid> {
    row.get(key)
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// The entity record of a row of an entity collection.
fn entity_record(coll: &Collection, row: &Value) -> Option<EntityRecord> {
    let e = coll.def.entity.as_ref()?;
    let id = row_id(row).ok()?;
    let m = row.as_object()?;
    let field_text = |name: &Option<String>| -> Option<String> {
        let n = name.as_ref()?;
        let f = coll.def.field(n)?;
        let t = text_of(f, m.get(n)?);
        if t.is_empty() { None } else { Some(t) }
    };
    let space = opt_uuid(row, "spaceId");
    let user = opt_uuid(row, "userId");
    let parent = e
        .parent
        .as_ref()
        .and_then(|p| m.get(p))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| container_uri(coll, space, user));
    let private = coll.def.scope == Scope::User
        || e.private
            .as_ref()
            .and_then(|p| m.get(p))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let mut search = String::new();
    for f in coll
        .def
        .fields
        .iter()
        .filter(|f| f.search && f.name != e.title)
    {
        if let Some(v) = m.get(&f.name) {
            let t = text_of(f, v);
            if !t.is_empty() {
                search.push_str(&t);
                search.push(' ');
            }
        }
    }
    Some(EntityRecord {
        uri: format!("{}://{id}", e.scheme),
        space_id: space,
        parent: Some(parent),
        title: field_text(&Some(e.title.clone())).unwrap_or_default(),
        subtitle: field_text(&e.subtitle),
        icon: field_text(&e.icon),
        search_text: Some(search),
        owner_id: user.or_else(|| opt_uuid(row, "createdBy")),
        private,
        plugin: Some(coll.plugin.clone()),
    })
}

fn ref_source(coll: &Collection, row: &Value) -> Result<RefSource> {
    let id = row_id(row)?;
    let space = opt_uuid(row, "spaceId");
    let user = opt_uuid(row, "userId");
    Ok(match &coll.def.entity {
        Some(e) => RefSource {
            scheme: e.scheme.clone(),
            id: id.to_string(),
            access_uri: format!("{}://{id}", e.scheme),
            owner_id: None,
            plugin: Some(coll.plugin.clone()),
            collection: Some(coll.def.name.clone()),
        },
        None => RefSource {
            scheme: "row".into(),
            id: format!("{}/{}/{id}", coll.plugin, coll.def.name),
            access_uri: container_uri(coll, space, user),
            owner_id: user,
            plugin: Some(coll.plugin.clone()),
            collection: Some(coll.def.name.clone()),
        },
    })
}

/// The URIs a field's value references.
fn ref_targets(f: &FieldDef, v: &Value) -> Vec<Uri> {
    let strs: Vec<String> = match (&f.ty, v) {
        (FieldType::Blob, Value::String(s)) => vec![format!("blob://{s}")],
        (FieldType::Ref { .. }, Value::String(s)) => vec![s.clone()],
        (FieldType::Refs { .. }, Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        (FieldType::Richtext, doc) if doc.is_object() => richtext::mentions(doc),
        _ => vec![],
    };
    let mut out: Vec<Uri> = Vec::new();
    for s in strs {
        if let Ok(u) = Uri::parse(&s)
            && !out.contains(&u)
        {
            out.push(u);
        }
    }
    out
}

fn has_refs(f: &FieldDef) -> bool {
    matches!(
        f.ty,
        FieldType::Blob | FieldType::Ref { .. } | FieldType::Refs { .. } | FieldType::Richtext
    )
}

fn user_mentions(f: &FieldDef, v: Option<&Value>) -> Vec<Uuid> {
    match (&f.ty, v) {
        (FieldType::Richtext, Some(doc)) => richtext::mentions(doc)
            .into_iter()
            .filter_map(|u| {
                u.strip_prefix("user://")
                    .and_then(|id| Uuid::parse_str(id).ok())
            })
            .collect(),
        _ => vec![],
    }
}

/// Updates the entity index and the reference index for a row. `changed` limits the
/// reference update to those fields (all when `None`). Returns new user mentions.
pub(crate) async fn index_row(
    conn: &mut PgConnection,
    coll: &Collection,
    row: &Value,
    old: Option<&Value>,
    changed: Option<&[String]>,
) -> Result<Vec<Mention>> {
    if let Some(rec) = entity_record(coll, row) {
        crate::entities::upsert(
            &mut *conn,
            &rec,
            Some(CollectionRef {
                plugin: &coll.plugin,
                collection: &coll.def.name,
            }),
        )
        .await?;
    }
    if !is_deleted(row) {
        check_blobs(conn, coll, None, row).await?;
    }
    let src = ref_source(coll, row)?;
    let id = row_id(row)?;
    let mut mentions = Vec::new();
    for f in coll.def.fields.iter().filter(|f| has_refs(f)) {
        if changed.is_some_and(|c| !c.contains(&f.name)) {
            continue;
        }
        let v = row.get(&f.name).unwrap_or(&Value::Null);
        crate::refs::replace(&mut *conn, &src, &f.name, &ref_targets(f, v), old.is_none()).await?;
        let before = user_mentions(f, old.and_then(|o| o.get(&f.name)));
        for u in user_mentions(f, Some(v)) {
            if !before.contains(&u) {
                mentions.push(Mention {
                    user: u,
                    plugin: coll.plugin.clone(),
                    collection: coll.def.name.clone(),
                    row_id: id,
                    field: f.name.clone(),
                    entity: coll.entity_uri(id),
                });
            }
        }
    }
    Ok(mentions)
}

async fn record_history(
    conn: &mut PgConnection,
    coll: &Collection,
    caller: &Caller,
    id: Uuid,
    rev: i64,
    op: ChangeOp,
    changes: Value,
) -> Result<()> {
    if !coll.def.history {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO rookery.history (plugin, collection, row_id, rev, op, changes, principal, on_behalf_of) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(&coll.plugin)
    .bind(&coll.def.name)
    .bind(id)
    .bind(rev)
    .bind(op.as_str())
    .bind(changes)
    .bind(caller.actor())
    .bind(caller.on_behalf_of_user())
    .execute(conn)
    .await?;
    Ok(())
}

fn pending(
    coll: &Collection,
    op: ChangeOp,
    row: Option<&Value>,
    id: Uuid,
    rev: i64,
    space: Option<Uuid>,
    user: Option<Uuid>,
) -> PendingChange {
    PendingChange {
        plugin: coll.plugin.clone(),
        collection: coll.def.name.clone(),
        id,
        op,
        rev,
        space_id: space,
        user_id: user,
        row: row.cloned(),
    }
}

fn rev_of(row: &Value) -> i64 {
    row.get("rev").and_then(Value::as_i64).unwrap_or(0)
}

pub(crate) async fn require(
    conn: &mut PgConnection,
    caller: &Caller,
    uri: &str,
    level: Level,
) -> Result<()> {
    crate::access::require(conn, caller, uri, level).await
}

/// The URI write access is checked on for an existing row.
fn access_target(coll: &Collection, row: &Value) -> Result<String> {
    let id = row_id(row)?;
    Ok(coll
        .entity_uri(id)
        .unwrap_or_else(|| container_uri(coll, opt_uuid(row, "spaceId"), opt_uuid(row, "userId"))))
}

pub(crate) async fn insert(
    w: &mut WriteTx,
    coll: &Collection,
    caller: &Caller,
    values: &Map<String, Value>,
) -> Result<Value> {
    lock_unique(w, coll).await?;
    let scope = write_scope(coll, caller)?;
    let mut id = new_id();
    let mut cols: Vec<(&FieldDef, Bind)> = Vec::new();
    let mut given: Map<String, Value> = Map::new();
    for (k, v) in values {
        if k == "id" {
            id = v
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| Error::invalid("id must be a UUID"))?;
            continue;
        }
        let f = coll
            .def
            .field(k)
            .ok_or_else(|| Error::invalid(format!("unknown field {k}")))?;
        if matches!(f.ty, FieldType::Sequence { .. }) {
            return Err(Error::invalid(format!(
                "field {k} is assigned by the store"
            )));
        }
        let b = input(&coll.def.name, f, v)?;
        let val = canonical(f, &b, v);
        constraints::check_field_value(&coll.def.name, f, &val).map_err(Error::from)?;
        given.insert(k.clone(), val);
        cols.push((f, b));
    }
    for f in &coll.def.fields {
        if given.contains_key(&f.name) || matches!(f.ty, FieldType::Sequence { .. }) {
            continue;
        }
        if let Some(d) = &f.default {
            let b = input(&coll.def.name, f, d)?;
            let val = canonical(f, &b, d);
            constraints::check_field_value(&coll.def.name, f, &val).map_err(Error::from)?;
            given.insert(f.name.clone(), val);
            cols.push((f, b));
        } else {
            constraints::check_field_value(&coll.def.name, f, &Value::Null).map_err(Error::from)?;
        }
    }
    if checks_access(coll, caller) {
        let parent = coll
            .def
            .entity
            .as_ref()
            .and_then(|e| e.parent.as_ref())
            .and_then(|p| given.get(p))
            .and_then(Value::as_str)
            .map(str::to_string);
        let target = parent.unwrap_or_else(|| container_uri(coll, scope.space, scope.user));
        require(&mut w.tx, caller, &target, coll.def.access.write).await?;
    }
    for f in &coll.def.fields {
        if let FieldType::Sequence { per } = &f.ty {
            let key = match per.unwrap_or(coll.def.scope) {
                Scope::Global => String::new(),
                Scope::Space => scope.space.map(|s| s.to_string()).unwrap_or_default(),
                Scope::User => scope.user.map(|u| u.to_string()).unwrap_or_default(),
            };
            let n: i64 = sqlx::query_scalar(
                "INSERT INTO rookery.sequences (plugin, collection, field, scope_key, value) VALUES ($1, $2, $3, $4, 1) \
                 ON CONFLICT (plugin, collection, field, scope_key) DO UPDATE SET value = rookery.sequences.value + 1 \
                 RETURNING value",
            )
            .bind(&coll.plugin)
            .bind(&coll.def.name)
            .bind(&f.name)
            .bind(key)
            .fetch_one(&mut *w.tx)
            .await?;
            given.insert(f.name.clone(), json!(n));
            cols.push((f, Bind::Int(n)));
        }
    }

    let mut qb = QueryBuilder::<Postgres>::new(format!(
        "INSERT INTO {} AS t (id, space_id, user_id, created_by, updated_by, _search",
        coll.table
    ));
    for (f, _) in &cols {
        qb.push(", ");
        qb.push(quote_ident(&f.name));
    }
    qb.push(") VALUES (");
    qb.push_bind(id);
    qb.push(", ");
    qb.push_bind(scope.space);
    qb.push(", ");
    qb.push_bind(scope.user);
    qb.push(", ");
    qb.push_bind(caller.user);
    qb.push(", ");
    qb.push_bind(caller.user);
    qb.push(", ");
    push_search_expr(&mut qb, search_texts(coll, &given));
    for (f, b) in cols {
        qb.push(", ");
        b.push(&mut qb, &f.ty.sql_type());
    }
    qb.push(") RETURNING to_jsonb(t) - '_search' AS row");
    let r = qb
        .build()
        .fetch_one(&mut *w.tx)
        .await
        .map_err(|e| unique_message(coll, e))?;
    let row = row_out(coll, r.try_get("row")?);

    check_blobs(&mut w.tx, coll, Some(caller), &row).await?;
    let mentions = index_row(&mut w.tx, coll, &row, None, None).await?;
    w.mentions.extend(mentions);
    if coll.def.history {
        let mut changes = Map::new();
        for f in &coll.def.fields {
            if let Some(v) = row.get(&f.name)
                && !v.is_null()
            {
                changes.insert(
                    f.name.clone(),
                    if matches!(f.ty, FieldType::Bytes { .. }) {
                        json!({"changed": true})
                    } else {
                        json!([null, v])
                    },
                );
            }
        }
        record_history(
            &mut w.tx,
            coll,
            caller,
            id,
            1,
            ChangeOp::Insert,
            Value::Object(changes),
        )
        .await?;
    }
    w.pending.push(pending(
        coll,
        ChangeOp::Insert,
        Some(&row),
        id,
        rev_of(&row),
        scope.space,
        scope.user,
    ));
    Ok(row)
}

/// A JSON form of a bound value close enough to the stored form for text extraction.
pub(crate) fn canonical(f: &FieldDef, b: &Bind, original: &Value) -> Value {
    if matches!(f.ty, FieldType::Bytes { .. } | FieldType::Vector { .. }) {
        return original.clone();
    }
    match b {
        Bind::Text(s) => Value::String(s.clone()),
        Bind::TextArr(a) => json!(a),
        _ => original.clone(),
    }
}

fn unique_message(coll: &Collection, e: sqlx::Error) -> Error {
    let err = Error::from(e);
    if err.is_unique_violation() {
        Error::conflict(format!(
            "a row of {} with the same unique value exists",
            coll.def.name
        ))
    } else {
        err
    }
}

/// Locks and returns a row in the caller's scope.
async fn lock_row(
    conn: &mut PgConnection,
    coll: &Collection,
    caller: &Caller,
    id: Uuid,
) -> Result<Value> {
    let scope = write_scope(coll, caller)?;
    let mut qb = QueryBuilder::<Postgres>::new(format!(
        "SELECT to_jsonb(t) - '_search' AS row FROM {} t WHERE t.id = ",
        coll.table
    ));
    qb.push_bind(id);
    if let Some(s) = scope.space {
        qb.push(" AND t.space_id = ");
        qb.push_bind(s);
    }
    if let Some(u) = scope.user {
        qb.push(" AND t.user_id = ");
        qb.push_bind(u);
    }
    qb.push(" FOR UPDATE");
    match qb.build().fetch_optional(conn).await? {
        Some(r) => Ok(row_out(coll, r.try_get("row")?)),
        None => Err(Error::not_found(format!("no {} row {id}", coll.def.name))),
    }
}

fn is_deleted(row: &Value) -> bool {
    row.get("deletedAt").is_some_and(|v| !v.is_null())
}

pub(crate) async fn update(
    w: &mut WriteTx,
    coll: &Collection,
    caller: &Caller,
    id: Uuid,
    patch: &Map<String, Value>,
    expected_rev: Option<i64>,
) -> Result<Value> {
    update_inner(w, coll, caller, id, patch, expected_rev, false).await
}

#[allow(clippy::too_many_arguments)]
async fn update_inner(
    w: &mut WriteTx,
    coll: &Collection,
    caller: &Caller,
    id: Uuid,
    patch: &Map<String, Value>,
    expected_rev: Option<i64>,
    force: bool,
) -> Result<Value> {
    if patch.keys().any(|name| unique_field(coll, name)) {
        lock_unique(w, coll).await?;
    }
    let old = lock_row(&mut w.tx, coll, caller, id).await?;
    if is_deleted(&old) {
        return Err(Error::not_found(format!(
            "{} row {id} is in the trash",
            coll.def.name
        )));
    }
    let rev = rev_of(&old);
    if let Some(expected) = expected_rev
        && expected != rev
    {
        return Err(Error::RevConflict {
            expected,
            actual: rev,
        });
    }
    let mut cols: Vec<(&FieldDef, Bind)> = Vec::new();
    let mut merged = old.as_object().cloned().unwrap_or_default();
    for (k, v) in patch {
        let f = coll
            .def
            .field(k)
            .ok_or_else(|| Error::invalid(format!("unknown or read-only field {k}")))?;
        if matches!(f.ty, FieldType::Sequence { .. }) {
            return Err(Error::invalid(format!(
                "field {k} is assigned by the store"
            )));
        }
        let b = input(&coll.def.name, f, v)?;
        let val = canonical(f, &b, v);
        constraints::check_field_value(&coll.def.name, f, &val).map_err(Error::from)?;
        let prev = old.get(k).unwrap_or(&Value::Null);
        constraints::check_transition(&coll.def.name, f, prev, &val).map_err(Error::from)?;
        merged.insert(k.clone(), val);
        cols.push((f, b));
    }
    if checks_access(coll, caller) {
        require(
            &mut w.tx,
            caller,
            &access_target(coll, &old)?,
            coll.def.access.write,
        )
        .await?;
        if let Some(p) = coll.def.entity.as_ref().and_then(|e| e.parent.as_ref())
            && let Some(newp) = merged.get(p).and_then(Value::as_str)
            && old.get(p).and_then(Value::as_str) != Some(newp)
        {
            require(&mut w.tx, caller, newp, coll.def.access.write).await?;
        }
    }
    if cols.is_empty() {
        return Ok(old);
    }
    let touches_search = cols.iter().any(|(f, _)| f.search);
    let mut qb = QueryBuilder::<Postgres>::new(format!(
        "UPDATE {} AS t SET rev = t.rev + 1, updated_at = now(), updated_by = ",
        coll.table
    ));
    qb.push_bind(caller.user);
    if touches_search {
        qb.push(", _search = ");
        push_search_expr(&mut qb, search_texts(coll, &merged));
    }
    let mut distinct = Vec::new();
    for (f, b) in cols {
        let col = quote_ident(&f.name);
        qb.push(format!(", {col} = "));
        b.clone().push(&mut qb, &f.ty.sql_type());
        distinct.push((col, b, f.ty.sql_type()));
    }
    qb.push(" WHERE t.id = ");
    qb.push_bind(id);
    // Only write when a value actually differs.
    qb.push(if force { " AND (TRUE OR " } else { " AND (" });
    for (i, (col, b, ty)) in distinct.into_iter().enumerate() {
        if i > 0 {
            qb.push(" OR ");
        }
        qb.push(format!("t.{col} IS DISTINCT FROM "));
        b.push(&mut qb, &ty);
    }
    qb.push(") RETURNING to_jsonb(t) - '_search' AS row");
    let Some(r) = qb
        .build()
        .fetch_optional(&mut *w.tx)
        .await
        .map_err(|e| unique_message(coll, e))?
    else {
        return Ok(old);
    };
    let row = row_out(coll, r.try_get("row")?);
    let changed: Vec<String> = patch
        .keys()
        .filter(|k| old.get(*k) != row.get(*k))
        .cloned()
        .collect();
    check_blobs(&mut w.tx, coll, Some(caller), &row).await?;
    let mentions = index_row(&mut w.tx, coll, &row, Some(&old), Some(&changed)).await?;
    w.mentions.extend(mentions);
    if coll.def.history {
        let mut changes = Map::new();
        for k in &changed {
            changes.insert(
                k.clone(),
                if coll
                    .def
                    .field(k)
                    .is_some_and(|f| matches!(f.ty, FieldType::Bytes { .. }))
                {
                    json!({"changed": true})
                } else {
                    json!([old.get(k), row.get(k)])
                },
            );
        }
        record_history(
            &mut w.tx,
            coll,
            caller,
            id,
            rev_of(&row),
            ChangeOp::Update,
            Value::Object(changes),
        )
        .await?;
    }
    w.pending.push(pending(
        coll,
        ChangeOp::Update,
        Some(&row),
        id,
        rev_of(&row),
        opt_uuid(&row, "spaceId"),
        opt_uuid(&row, "userId"),
    ));
    Ok(row)
}

/// Deletes a row: into the trash when the collection has one, otherwise for good.
pub(crate) async fn delete(
    w: &mut WriteTx,
    coll: &Collection,
    caller: &Caller,
    id: Uuid,
) -> Result<Option<Value>> {
    let old = lock_row(&mut w.tx, coll, caller, id).await?;
    if is_deleted(&old) {
        return Err(Error::not_found(format!(
            "{} row {id} is already in the trash",
            coll.def.name
        )));
    }
    if checks_access(coll, caller) {
        require(
            &mut w.tx,
            caller,
            &access_target(coll, &old)?,
            coll.def.access.delete_level(),
        )
        .await?;
    }
    if coll.def.trash {
        let mut qb = QueryBuilder::<Postgres>::new(format!(
            "UPDATE {} AS t SET deleted_at = now(), rev = t.rev + 1, updated_at = now(), updated_by = ",
            coll.table
        ));
        qb.push_bind(caller.user);
        qb.push(" WHERE t.id = ");
        qb.push_bind(id);
        qb.push(" RETURNING to_jsonb(t) - '_search' AS row");
        let r = qb.build().fetch_one(&mut *w.tx).await?;
        let row = row_out(coll, r.try_get("row")?);
        set_row_deleted(&mut w.tx, coll, &row, true).await?;
        record_history(
            &mut w.tx,
            coll,
            caller,
            id,
            rev_of(&row),
            ChangeOp::Delete,
            json!({}),
        )
        .await?;
        w.pending.push(pending(
            coll,
            ChangeOp::Delete,
            Some(&row),
            id,
            rev_of(&row),
            opt_uuid(&row, "spaceId"),
            opt_uuid(&row, "userId"),
        ));
        Ok(Some(row))
    } else {
        hard_delete(w, coll, caller, &old).await?;
        Ok(None)
    }
}

async fn hard_delete(
    w: &mut WriteTx,
    coll: &Collection,
    caller: &Caller,
    old: &Value,
) -> Result<()> {
    let id = row_id(old)?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE FROM {} WHERE id = $1",
        coll.table
    )))
    .bind(id)
    .execute(&mut *w.tx)
    .await?;
    match coll.entity_uri(id) {
        Some(uri) => {
            crate::entities::delete(&mut w.tx, &uri).await?;
        }
        None => {
            let src = ref_source(coll, old)?;
            crate::refs::delete_from(&mut w.tx, &src.scheme, &src.id).await?;
        }
    }
    record_history(
        &mut w.tx,
        coll,
        caller,
        id,
        rev_of(old) + 1,
        ChangeOp::Delete,
        json!({ "purged": true }),
    )
    .await?;
    w.pending.push(pending(
        coll,
        ChangeOp::Delete,
        None,
        id,
        rev_of(old) + 1,
        opt_uuid(old, "spaceId"),
        opt_uuid(old, "userId"),
    ));
    Ok(())
}

pub(crate) async fn set_row_deleted(
    conn: &mut PgConnection,
    coll: &Collection,
    row: &Value,
    deleted: bool,
) -> Result<()> {
    let id = row_id(row)?;
    match coll.def.entity.as_ref() {
        Some(e) => {
            crate::entities::set_deleted(conn, &Uri::new(e.scheme.clone(), id.to_string()), deleted)
                .await
        }
        None => {
            let src = ref_source(coll, row)?;
            crate::refs::set_source_deleted(conn, &src.scheme, &src.id, deleted).await
        }
    }
}

/// Takes a row out of the trash.
pub(crate) async fn restore(
    w: &mut WriteTx,
    coll: &Collection,
    caller: &Caller,
    id: Uuid,
) -> Result<Value> {
    if !coll.def.trash {
        return Err(Error::invalid(format!(
            "collection {} has no trash",
            coll.def.name
        )));
    }
    let old = lock_row(&mut w.tx, coll, caller, id).await?;
    if !is_deleted(&old) {
        return Err(Error::not_found(format!(
            "{} row {id} is not in the trash",
            coll.def.name
        )));
    }
    if checks_access(coll, caller) {
        require(
            &mut w.tx,
            caller,
            &access_target(coll, &old)?,
            coll.def.access.delete_level(),
        )
        .await?;
    }
    let mut qb = QueryBuilder::<Postgres>::new(format!(
        "UPDATE {} AS t SET deleted_at = NULL, rev = t.rev + 1, updated_at = now(), updated_by = ",
        coll.table
    ));
    qb.push_bind(caller.user);
    qb.push(" WHERE t.id = ");
    qb.push_bind(id);
    qb.push(" RETURNING to_jsonb(t) - '_search' AS row");
    let r = qb
        .build()
        .fetch_one(&mut *w.tx)
        .await
        .map_err(|e| unique_message(coll, e))?;
    let row = row_out(coll, r.try_get("row")?);
    check_blobs(&mut w.tx, coll, Some(caller), &row).await?;
    set_row_deleted(&mut w.tx, coll, &row, false).await?;
    record_history(
        &mut w.tx,
        coll,
        caller,
        id,
        rev_of(&row),
        ChangeOp::Restore,
        json!({}),
    )
    .await?;
    w.pending.push(pending(
        coll,
        ChangeOp::Restore,
        Some(&row),
        id,
        rev_of(&row),
        opt_uuid(&row, "spaceId"),
        opt_uuid(&row, "userId"),
    ));
    Ok(row)
}

/// Deletes a row in the trash for good. Needs `manage`.
pub(crate) async fn purge(
    w: &mut WriteTx,
    coll: &Collection,
    caller: &Caller,
    id: Uuid,
) -> Result<()> {
    let old = lock_row(&mut w.tx, coll, caller, id).await?;
    if !is_deleted(&old) {
        return Err(Error::not_found(format!(
            "{} row {id} is not in the trash",
            coll.def.name
        )));
    }
    if checks_access(coll, caller) {
        require(
            &mut w.tx,
            caller,
            &access_target(coll, &old)?,
            Level::Manage,
        )
        .await?;
    }
    hard_delete(w, coll, caller, &old).await
}

async fn check_blobs(
    conn: &mut PgConnection,
    coll: &Collection,
    caller: Option<&Caller>,
    row: &Value,
) -> Result<()> {
    if !coll
        .def
        .fields
        .iter()
        .any(|f| matches!(f.ty, FieldType::Blob))
    {
        return Ok(());
    }
    // Serialize reference creation with collection/deletion; collectors must see committed references.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('rookery.blobs.references'))")
        .execute(&mut *conn)
        .await?;
    for f in coll
        .def
        .fields
        .iter()
        .filter(|f| matches!(f.ty, FieldType::Blob))
    {
        let Some(id) = row.get(&f.name).and_then(Value::as_str) else {
            continue;
        };
        let owner: Option<String> =
            sqlx::query_scalar("SELECT owner FROM rookery.blobs WHERE id = $1")
                .bind(Uuid::parse_str(id).map_err(|_| Error::invalid("invalid blob id"))?)
                .fetch_optional(&mut *conn)
                .await?;
        let Some(owner) = owner else {
            return Err(Error::not_found(format!(
                "blob {id} for {}.{} no longer exists",
                coll.def.name, f.name
            )));
        };
        // A blob is its putter's, besides admins and those it was shared with.
        if let Some(caller) = caller
            && !caller.admin
            && owner != caller.actor()
            && caller.user.is_none_or(|u| owner != format!("user:{u}"))
        {
            require(&mut *conn, caller, &format!("blob://{id}"), Level::View).await?;
        }
    }
    Ok(())
}

pub(crate) async fn update_ops(
    w: &mut WriteTx,
    coll: &Collection,
    caller: &Caller,
    id: Uuid,
    patch: &Map<String, Value>,
    rev: Option<i64>,
    ops: &super::operations::FieldOps,
) -> Result<Value> {
    if patch
        .keys()
        .chain(ops.keys())
        .any(|name| unique_field(coll, name))
    {
        lock_unique(w, coll).await?;
    }
    let old = lock_row(&mut w.tx, coll, caller, id).await?;
    if let Some(expected) = rev
        && expected != rev_of(&old)
    {
        return Err(Error::RevConflict {
            expected,
            actual: rev_of(&old),
        });
    }
    let patch = super::operations::apply(&coll.def, &old, patch, ops)?;
    update(w, coll, caller, id, &patch, rev).await
}

pub(crate) async fn upsert(
    w: &mut WriteTx,
    coll: &Collection,
    caller: &Caller,
    values: &Map<String, Value>,
    on: &[String],
) -> Result<Value> {
    if !is_unique_key(coll, on) {
        return Err(Error::invalid(
            "upsert on must name a declared unique constraint",
        ));
    }
    let scope = write_scope(coll, caller)?;
    lock_unique(w, coll).await?;
    let mut qb = QueryBuilder::<Postgres>::new(format!(
        "SELECT id FROM {} t WHERE deleted_at IS NULL",
        coll.table
    ));
    if let Some(space) = scope.space {
        qb.push(" AND space_id = ").push_bind(space);
    }
    if let Some(user) = scope.user {
        qb.push(" AND user_id = ").push_bind(user);
    }
    for name in on {
        let f = coll
            .def
            .field(name)
            .ok_or_else(|| Error::invalid(format!("unknown unique field {name}")))?;
        let v = values
            .get(name)
            .filter(|v| !v.is_null())
            .ok_or_else(|| Error::invalid(format!("upsert requires a non-null key {name}")))?;
        let b = input(&coll.def.name, f, v)?;
        let col = quote_ident(name);
        if f.case_insensitive {
            qb.push(format!(" AND lower({col}) = lower("));
            b.push(&mut qb, &f.ty.sql_type());
            qb.push(")");
        } else {
            qb.push(format!(" AND {col} = "));
            b.push(&mut qb, &f.ty.sql_type());
        }
    }
    qb.push(" FOR UPDATE");
    let id: Option<Uuid> = qb.build_query_scalar().fetch_optional(&mut *w.tx).await?;
    match id {
        Some(id) => update_inner(w, coll, caller, id, values, None, true).await,
        None => insert(w, coll, caller, values).await,
    }
}

/// Whether `on` names one unique field or exactly one composite unique constraint.
pub(crate) fn is_unique_key(coll: &Collection, on: &[String]) -> bool {
    !on.is_empty()
        && ((on.len() == 1 && coll.def.field(&on[0]).is_some_and(|f| f.unique))
            || coll.def.unique.iter().any(|u| u == on))
}

fn unique_field(coll: &Collection, name: &str) -> bool {
    coll.def.field(name).is_some_and(|f| f.unique)
        || coll
            .def
            .unique
            .iter()
            .any(|key| key.iter().any(|n| n == name))
}

/// Unique-key inserts and key changes participate in upsert arbitration too, so a concurrent
/// ordinary insert cannot turn an upsert into a spurious uniqueness conflict.
async fn lock_unique(w: &mut WriteTx, coll: &Collection) -> Result<()> {
    if coll.def.unique.is_empty() && !coll.def.fields.iter().any(|f| f.unique) {
        return Ok(());
    }
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(format!("rookery.upsert:{}.{}", coll.plugin, coll.def.name))
        .execute(&mut *w.tx)
        .await?;
    Ok(())
}
