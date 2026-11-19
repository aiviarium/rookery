//! Field-level history of collections with `history` on: which fields changed, from what to
//! what, by which principal on whose behalf. Read it through [`crate::collections::Data::history`],
//! which checks that the caller may read the row.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Store;
use crate::error::Result;

#[derive(Debug, Clone, Serialize, PartialEq, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: i64,
    pub rev: i64,
    /// `insert`, `update`, `delete` or `restore`.
    pub op: String,
    /// `{ "field": [from, to] }`.
    pub changes: Value,
    pub principal: String,
    pub on_behalf_of: Option<Uuid>,
    pub at: DateTime<Utc>,
}

pub struct History<'a> {
    pub(crate) store: &'a Store,
}

impl<'a> History<'a> {
    /// Without an access check; for the kernel's own use (admin views, audits).
    pub async fn for_row_unchecked(
        &self,
        plugin: &str,
        collection: &str,
        row: Uuid,
        limit: i64,
    ) -> Result<Vec<HistoryEntry>> {
        let mut conn = self.store.pool().acquire().await?;
        for_row(&mut conn, plugin, collection, row, limit).await
    }
}

pub(crate) async fn for_row(
    conn: &mut PgConnection,
    plugin: &str,
    collection: &str,
    row: Uuid,
    limit: i64,
) -> Result<Vec<HistoryEntry>> {
    Ok(sqlx::query_as(
        "SELECT id, rev, op, changes, principal, on_behalf_of, at FROM rookery.history \
         WHERE plugin = $1 AND collection = $2 AND row_id = $3 ORDER BY id DESC LIMIT $4",
    )
    .bind(plugin)
    .bind(collection)
    .bind(row)
    .bind(limit.clamp(1, 1000))
    .fetch_all(conn)
    .await?)
}
