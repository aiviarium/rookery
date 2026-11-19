//! Key-value storage of plugins. Every write gives the row a new revision, and a write may name the
//! revision it expects: 0 for a key that does not exist yet.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Entry {
    pub key: String,
    pub value: Value,
    pub revision: i64,
    pub updated_at: DateTime<Utc>,
}

pub struct Kv<'a> {
    pub(crate) pool: &'a PgPool,
}

fn parts(user: Option<Uuid>) -> &'static str {
    if user.is_some() { "user" } else { "global" }
}

const WHERE: &str = "plugin = $1 AND scope = $2 AND user_id IS NOT DISTINCT FROM $3 AND key = $4";

fn check_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > 500 {
        return Err(Error::Invalid("a key is 1 to 500 bytes".into()));
    }
    Ok(())
}

impl Kv<'_> {
    /// `user` is `None` for the plugin's global scope.
    pub async fn get(&self, plugin: &str, user: Option<Uuid>, key: &str) -> Result<Option<Entry>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT key, value, revision, updated_at FROM rookery.kv WHERE {WHERE}"
        )))
        .bind(plugin)
        .bind(parts(user))
        .bind(user)
        .bind(key)
        .fetch_optional(self.pool)
        .await?)
    }

    /// Sets a value and returns its new revision. With `expected`, only if the key is at that revision.
    pub async fn set(
        &self,
        plugin: &str,
        user: Option<Uuid>,
        key: &str,
        value: &Value,
        expected: Option<i64>,
    ) -> Result<i64> {
        check_key(key)?;
        let found: Option<i64> = match expected {
            None => sqlx::query_scalar(
                "INSERT INTO rookery.kv (plugin, scope, user_id, key, value, revision) VALUES ($1, $2, $3, $4, $5, 1)
                 ON CONFLICT (plugin, scope, user_id, key) DO UPDATE SET value = EXCLUDED.value, revision = rookery.kv.revision + 1, updated_at = now()
                 RETURNING revision",
            )
            .bind(plugin)
            .bind(parts(user))
            .bind(user)
            .bind(key)
            .bind(value)
            .fetch_optional(self.pool)
            .await?,
            Some(0) => sqlx::query_scalar(
                "INSERT INTO rookery.kv (plugin, scope, user_id, key, value, revision) VALUES ($1, $2, $3, $4, $5, 1)
                 ON CONFLICT (plugin, scope, user_id, key) DO NOTHING RETURNING revision",
            )
            .bind(plugin)
            .bind(parts(user))
            .bind(user)
            .bind(key)
            .bind(value)
            .fetch_optional(self.pool)
            .await?,
            Some(revision) => sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "UPDATE rookery.kv SET value = $6, revision = revision + 1, updated_at = now() WHERE {WHERE} AND revision = $5 RETURNING revision"
            )))
            .bind(plugin)
            .bind(parts(user))
            .bind(user)
            .bind(key)
            .bind(revision)
            .bind(value)
            .fetch_optional(self.pool)
            .await?,
        };
        found.ok_or_else(|| match expected {
            Some(0) => Error::Conflict(format!("{key} exists already")),
            Some(revision) => Error::Conflict(format!("{key} is not at revision {revision}")),
            None => Error::Conflict(format!("{key} changed meanwhile")),
        })
    }

    /// Removes a key; with `expected`, only if it is at that revision.
    pub async fn delete(
        &self,
        plugin: &str,
        user: Option<Uuid>,
        key: &str,
        expected: Option<i64>,
    ) -> Result<bool> {
        let done = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM rookery.kv WHERE {WHERE} AND ($5::bigint IS NULL OR revision = $5)"
        )))
        .bind(plugin)
        .bind(parts(user))
        .bind(user)
        .bind(key)
        .bind(expected)
        .execute(self.pool)
        .await?;
        if done.rows_affected() == 0
            && let Some(revision) = expected
            && self.get(plugin, user, key).await?.is_some()
        {
            return Err(Error::Conflict(format!(
                "{key} is not at revision {revision}"
            )));
        }
        Ok(done.rows_affected() > 0)
    }

    /// The keys that start with `prefix`, in order, after `after`.
    pub async fn list(
        &self,
        plugin: &str,
        user: Option<Uuid>,
        prefix: &str,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Entry>> {
        Ok(sqlx::query_as(
            "SELECT key, value, revision, updated_at FROM rookery.kv
             WHERE plugin = $1 AND scope = $2 AND user_id IS NOT DISTINCT FROM $3 AND starts_with(key, $4) AND ($5::text IS NULL OR key > $5)
             ORDER BY key LIMIT $6",
        )
        .bind(plugin)
        .bind(parts(user))
        .bind(user)
        .bind(prefix)
        .bind(after)
        .bind(limit.clamp(1, 1000))
        .fetch_all(self.pool)
        .await?)
    }
}
