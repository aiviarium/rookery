//! `rookery.plugins`: the record of what is installed, at which version and from where. Hatchery's lock in
//! the plugin folder says what to install; this table is what the server found there, written after
//! every change and whenever it starts leading.

use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};
use rookery_proto::RpcError;
use rookery_store::Store;

use crate::loaded::{Loaded, origin_name};

/// A row of the table.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Row {
    pub id: String,
    pub package: String,
    pub version: String,
    pub origin: String,
    pub source: String,
    pub digest: String,
    pub client_hash: Option<String>,
    pub problem: Option<String>,
    pub installed_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Row {
    pub fn installed_at(&self) -> String {
        self.installed_at.to_rfc3339_opts(SecondsFormat::Secs, true)
    }
    pub fn updated_at(&self) -> String {
        self.updated_at.to_rfc3339_opts(SecondsFormat::Secs, true)
    }
}

fn failed(e: sqlx::Error) -> RpcError {
    RpcError::internal(format!("the table of plugins: {e}"))
}

/// Every row, by id.
pub async fn rows(store: &Store) -> Result<BTreeMap<String, Row>, RpcError> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, package, version, origin, source, digest, client_hash, problem, installed_at, updated_at
         FROM rookery.plugins ORDER BY id",
    )
    .fetch_all(store.pool())
    .await
    .map_err(failed)?;
    Ok(rows.into_iter().map(|row| (row.id.clone(), row)).collect())
}

/// Makes the table say what is installed: a new plugin gets a row, and one whose version or files
/// changed a new `updated_at`. The rows of `removed`, the plugins a change took out, go. The rows of
/// other plugins the folder lacks are kept, as on a new leader whose folder may be behind, and their
/// ids given back. It writes only while this server leads (the store's fence).
pub async fn sync(
    store: &Store,
    plugins: &[Loaded],
    removed: &[String],
) -> Result<Vec<String>, RpcError> {
    let mut tx = store.pool().begin().await.map_err(failed)?;
    sqlx::query("SELECT rookery.fence($1)")
        .bind(store.fence())
        .execute(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.code().as_deref() == Some("RK001") => {
                RpcError::unavailable("this server does not lead any more")
            }
            _ => failed(e),
        })?;
    let ids: Vec<String> = plugins.iter().map(|p| p.id.clone()).collect();
    let removed: Vec<&String> = removed.iter().filter(|id| !ids.contains(id)).collect();
    sqlx::query("DELETE FROM rookery.plugins WHERE id = ANY($1)")
        .bind(&removed)
        .execute(&mut *tx)
        .await
        .map_err(failed)?;
    let missing: Vec<String> =
        sqlx::query_scalar("SELECT id FROM rookery.plugins WHERE NOT (id = ANY($1)) ORDER BY id")
            .bind(&ids)
            .fetch_all(&mut *tx)
            .await
            .map_err(failed)?;
    for plugin in plugins {
        sqlx::query(
            "INSERT INTO rookery.plugins (id, package, version, origin, source, digest, client_hash, problem)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (id) DO UPDATE SET
               package = EXCLUDED.package, version = EXCLUDED.version, origin = EXCLUDED.origin,
               source = EXCLUDED.source, digest = EXCLUDED.digest, client_hash = EXCLUDED.client_hash,
               problem = EXCLUDED.problem,
               updated_at = CASE
                 WHEN rookery.plugins.version IS DISTINCT FROM EXCLUDED.version
                   OR rookery.plugins.digest IS DISTINCT FROM EXCLUDED.digest
                   OR rookery.plugins.package IS DISTINCT FROM EXCLUDED.package
                 THEN now() ELSE rookery.plugins.updated_at END",
        )
        .bind(&plugin.id)
        .bind(&plugin.package)
        .bind(&plugin.version)
        .bind(origin_name(plugin.origin))
        .bind(&plugin.source)
        .bind(&plugin.digest)
        .bind(plugin.client.as_ref().map(|c| c.hash.clone()))
        .bind(plugin.problem.as_ref().map(|p| p.message.clone()))
        .execute(&mut *tx)
        .await
        .map_err(failed)?;
    }
    tx.commit().await.map_err(failed)?;
    Ok(missing)
}
