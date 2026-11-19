//! The audit log: who did what to which resource.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::error::Result;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Entry {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub principal: String,
    pub action: String,
    pub resource: Option<String>,
    pub details: Value,
}

/// What [`Audit::list`] returns.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub resource: Option<String>,
    pub principal: Option<String>,
    pub action: Option<String>,
    /// Entries with a smaller id only, to page back.
    pub before: Option<i64>,
    pub limit: i64,
}

pub struct Audit<'a> {
    pub(crate) pool: &'a PgPool,
}

impl Audit<'_> {
    pub async fn record(
        &self,
        principal: &str,
        action: &str,
        resource: Option<&str>,
        details: Value,
    ) -> Result<()> {
        sqlx::query("INSERT INTO rookery.audit (principal, action, resource, details) VALUES ($1, $2, $3, $4)")
            .bind(principal)
            .bind(action)
            .bind(resource)
            .bind(details)
            .execute(self.pool)
            .await?;
        Ok(())
    }

    /// The newest entries first.
    pub async fn list(&self, filter: &Filter) -> Result<Vec<Entry>> {
        Ok(sqlx::query_as(
            "SELECT id, at, principal, action, resource, details FROM rookery.audit
             WHERE ($1::text IS NULL OR resource = $1) AND ($2::text IS NULL OR principal = $2)
               AND ($3::text IS NULL OR action = $3) AND ($4::bigint IS NULL OR id < $4)
             ORDER BY id DESC LIMIT $5",
        )
        .bind(&filter.resource)
        .bind(&filter.principal)
        .bind(&filter.action)
        .bind(filter.before)
        .bind(filter.limit.clamp(1, 500))
        .fetch_all(self.pool)
        .await?)
    }
}

/// Adds an entry in a transaction of the caller's, so that it commits with the change it records.
pub(crate) async fn record_on(
    conn: &mut sqlx::PgConnection,
    principal: &str,
    action: &str,
    resource: Option<&str>,
    details: Value,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO rookery.audit (principal, action, resource, details) VALUES ($1, $2, $3, $4)",
    )
    .bind(principal)
    .bind(action)
    .bind(resource)
    .bind(details)
    .execute(conn)
    .await?;
    Ok(())
}
