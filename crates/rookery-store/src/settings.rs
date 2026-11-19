//! Settings of plugins. A value resolves as the user's, else the space's, else the global one, else the
//! plugin's default.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Default,
    Global,
    Space(Uuid),
    User(Uuid),
}

impl Scope {
    fn parts(self) -> (&'static str, Option<Uuid>, Option<Uuid>) {
        match self {
            Scope::Default => ("default", None, None),
            Scope::Global => ("global", None, None),
            Scope::Space(space) => ("space", None, Some(space)),
            Scope::User(user) => ("user", Some(user), None),
        }
    }
}

/// A resolved value and where it came from: `default`, `global`, `space` or `user`.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Resolved {
    pub key: String,
    pub value: Value,
    pub scope: String,
    pub updated_at: DateTime<Utc>,
}

pub struct Settings<'a> {
    pub(crate) pool: &'a PgPool,
}

const RESOLVE: &str = "SELECT DISTINCT ON (key) key, value, scope, updated_at FROM rookery.settings
     WHERE plugin = $1 AND (scope IN ('default', 'global') OR (scope = 'user' AND user_id = $2)
         OR (scope = 'space' AND space_id = $4))";
const ORDER: &str = "ORDER BY key, CASE scope WHEN 'user' THEN 0 WHEN 'space' THEN 1 WHEN 'global' THEN 2 ELSE 3 END";

impl Settings<'_> {
    pub async fn set(
        &self,
        plugin: &str,
        key: &str,
        scope: Scope,
        value: &Value,
        by: &str,
    ) -> Result<()> {
        let (scope, user, space) = scope.parts();
        let mut tx = crate::actor::begin_as(self.pool, by).await?;
        sqlx::query(
            "INSERT INTO rookery.settings (plugin, key, scope, user_id, space_id, value, updated_by) VALUES ($1, $2, $3, $4, $7, $5, $6)
             ON CONFLICT ON CONSTRAINT settings_key DO UPDATE SET value = EXCLUDED.value, updated_by = EXCLUDED.updated_by, updated_at = now()",
        )
        .bind(plugin)
        .bind(key)
        .bind(scope)
        .bind(user)
        .bind(value)
        .bind(by)
        .bind(space)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Removes the value at one scope, so that the next one shows through.
    pub async fn reset(&self, plugin: &str, key: &str, scope: Scope) -> Result<bool> {
        let (scope, user, space) = scope.parts();
        let mut tx = crate::actor::begin(self.pool).await?;
        let done = sqlx::query(
            "DELETE FROM rookery.settings WHERE plugin = $1 AND key = $2 AND scope = $3 AND user_id IS NOT DISTINCT FROM $4
                 AND space_id IS NOT DISTINCT FROM $5",
        )
        .bind(plugin)
        .bind(key)
        .bind(scope)
        .bind(user)
        .bind(space)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn get(
        &self,
        plugin: &str,
        key: &str,
        user: Option<Uuid>,
    ) -> Result<Option<Resolved>> {
        self.get_in(plugin, key, user, None).await
    }

    /// A value as it resolves for a person in a space.
    pub async fn get_in(
        &self,
        plugin: &str,
        key: &str,
        user: Option<Uuid>,
        space: Option<Uuid>,
    ) -> Result<Option<Resolved>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{RESOLVE} AND key = $3 {ORDER}"
        )))
        .bind(plugin)
        .bind(user)
        .bind(key)
        .bind(space)
        .fetch_optional(self.pool)
        .await?)
    }

    pub async fn list(&self, plugin: &str, user: Option<Uuid>) -> Result<Vec<Resolved>> {
        self.list_in(plugin, user, None).await
    }

    /// Every value of a plugin as it resolves for a person in a space.
    pub async fn list_in(
        &self,
        plugin: &str,
        user: Option<Uuid>,
        space: Option<Uuid>,
    ) -> Result<Vec<Resolved>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{RESOLVE} AND $3::text IS NULL {ORDER}"
        )))
        .bind(plugin)
        .bind(user)
        .bind(None::<String>)
        .bind(space)
        .fetch_all(self.pool)
        .await?)
    }
}
