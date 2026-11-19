//! Bulk operations: `scan`, a full pass over a collection, and `import`, bulk insert.
//!
//! **Scan** is `find` in one fixed order: `id` (a UUIDv7 unless the writer chose the id, so mostly the order rows
//! were created in), after a cursor. It is a loop of ordinary pages, not one transaction: each page is a consistent
//! read, and the pass as a whole is stable: a row that exists for the whole pass is returned exactly once, a row
//! deleted before the pass reaches it is not returned, and a row inserted after the pass has passed its id is not.
//!
//! **Import** writes up to [`MAX_IMPORT_ROWS`] rows in one transaction. A row that is refused (a violated
//! constraint, a unique conflict, a missing blob) is rolled back alone, to a savepoint, and reported by its index;
//! the others commit together, each with the change log entry, history and indexes an ordinary write has, and the
//! log entries are numbered in one block under one take of the ordering lock. Anything that is not about one row
//! (a lost connection, no access to the collection) fails the call and writes nothing.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::Collection;
use super::query::{MAX_LIMIT, Query};
use super::write::{self, WriteTx};
use crate::Store;
use crate::caller::Caller;
use crate::error::{Error, Result};

/// The most rows one `import` call takes: one transaction.
pub const MAX_IMPORT_ROWS: usize = 500;

/// `scan`'s query: the filter, the fields, a page size and a cursor. Nothing that sets an order.
pub(crate) fn scan_query(q: &Query) -> Result<Query> {
    if q.order_by.as_ref().is_some_and(|o| !o.is_empty())
        || q.search.is_some()
        || q.nearest.is_some()
        || q.include.is_some()
    {
        return Err(Error::invalid(
            "scan reads in id order: orderBy, search, nearest and include are not available",
        ));
    }
    Ok(Query {
        limit: Some(q.limit.unwrap_or(MAX_LIMIT)),
        ..q.clone()
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportMode {
    Insert,
    Upsert,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOptions {
    pub mode: ImportMode,
    /// `upsert`: the unique key, as for `upsert`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on: Vec<String>,
}

/// Why one row was not written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFailure {
    /// The row's position in the call's `rows`.
    pub index: usize,
    /// The error code: `INVALID`, `CONFLICT`, ...
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    pub inserted: u64,
    /// Rows `upsert` found and changed.
    pub updated: u64,
    pub failed: Vec<ImportFailure>,
}

/// Whether an error belongs to the row that caused it. Others (access, connections) fail the whole call.
fn about_one_row(e: &Error) -> bool {
    use rookery_proto::ErrorCode::*;
    match e {
        Error::Database(sqlx::Error::Database(d)) => d
            .code()
            .is_some_and(|c| c.starts_with("22") || c.starts_with("23")),
        _ => matches!(e.code(), Invalid | NotFound | Conflict),
    }
}

pub(crate) fn check_options(coll: &Collection, rows: usize, o: &ImportOptions) -> Result<()> {
    if rows > MAX_IMPORT_ROWS {
        return Err(Error::invalid(format!(
            "import takes at most {MAX_IMPORT_ROWS} rows per call"
        )));
    }
    match o.mode {
        ImportMode::Insert if !o.on.is_empty() => Err(Error::invalid("on is for mode upsert")),
        ImportMode::Upsert if coll.def.kind == super::CollectionKind::Log => Err(Error::invalid(
            "a log collection is append-only: import with mode insert",
        )),
        ImportMode::Upsert if !write::is_unique_key(coll, &o.on) => Err(Error::invalid(
            "upsert on must name a declared unique constraint",
        )),
        _ => Ok(()),
    }
}

/// Runs the import transaction. `idempotency` is the key of the call, recorded with its outcome.
pub(crate) async fn import(
    store: &Store,
    coll: &Collection,
    caller: &Caller,
    rows: &[Value],
    o: &ImportOptions,
    idempotency: Option<&crate::idempotency::Key>,
) -> Result<ImportOutcome> {
    let mut w = WriteTx::begin(store).await?;
    let mut out = ImportOutcome::default();
    for (index, row) in rows.iter().enumerate() {
        let values: Result<&Map<String, Value>> = row
            .as_object()
            .ok_or_else(|| Error::invalid("a row must be an object"));
        sqlx::query("SAVEPOINT import_row")
            .execute(&mut *w.tx)
            .await?;
        let (pending, mentions) = (w.pending.len(), w.mentions.len());
        let written = match (values, o.mode) {
            (Err(e), _) => Err(e),
            (Ok(v), ImportMode::Insert) => write::insert(&mut w, coll, caller, v).await,
            (Ok(v), ImportMode::Upsert) => write::upsert(&mut w, coll, caller, v, &o.on).await,
        };
        match written {
            Ok(row) => {
                sqlx::query("RELEASE SAVEPOINT import_row")
                    .execute(&mut *w.tx)
                    .await?;
                if row.get("rev").and_then(Value::as_i64) == Some(1) {
                    out.inserted += 1;
                } else {
                    out.updated += 1;
                }
            }
            Err(e) if about_one_row(&e) => {
                sqlx::query("ROLLBACK TO SAVEPOINT import_row")
                    .execute(&mut *w.tx)
                    .await?;
                w.pending.truncate(pending);
                w.mentions.truncate(mentions);
                out.failed.push(ImportFailure {
                    index,
                    code: e.code().as_str().to_string(),
                    message: e.to_string(),
                    details: e.details(),
                });
            }
            Err(e) => return Err(e),
        }
    }
    let response = serde_json::to_value(&out).map_err(|e| Error::internal(e.to_string()))?;
    if let Some(key) = idempotency
        && !crate::idempotency::record(&mut w.tx, key, &response).await?
    {
        drop(w);
        return replay(store, key)
            .await?
            .ok_or_else(|| Error::conflict("a write with this idempotency key is in progress"));
    }
    // Mentions are not sent for imported rows: a bulk load would notify people about thousands of rows.
    w.commit(store).await?;
    Ok(out)
}

/// The recorded outcome of an import with this key, if there is one.
pub(crate) async fn replay(
    store: &Store,
    key: &crate::idempotency::Key,
) -> Result<Option<ImportOutcome>> {
    let mut conn = store.pool().acquire().await?;
    Ok(match crate::idempotency::stored(&mut conn, key).await? {
        Some(r) => Some(serde_json::from_value(r).map_err(|e| Error::internal(e.to_string()))?),
        None => None,
    })
}
