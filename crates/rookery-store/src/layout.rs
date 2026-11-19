//! A person's layouts of a shell, by name, with a revision for conditional writes.

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{Error, Result};

pub struct Layout<'a> {
    pub(crate) pool: &'a PgPool,
}

impl Layout<'_> {
    pub async fn get(&self, user: Uuid, name: &str) -> Result<Option<(Value, i64)>> {
        Ok(sqlx::query_as(
            "SELECT value, revision FROM rookery.layouts WHERE user_id = $1 AND name = $2",
        )
        .bind(user)
        .bind(name)
        .fetch_optional(self.pool)
        .await?)
    }

    /// Writes a layout and returns its revision. With `expected`, only if it is at that one (0: new).
    pub async fn put(
        &self,
        user: Uuid,
        name: &str,
        value: &Value,
        expected: Option<i64>,
    ) -> Result<i64> {
        let sql = match expected {
            None => {
                "INSERT INTO rookery.layouts AS l (user_id, name, value, revision) VALUES ($1, $2, $3, 1)
                 ON CONFLICT (user_id, name) DO UPDATE SET value = EXCLUDED.value, revision = l.revision + 1, updated_at = now()
                 RETURNING revision"
            }
            Some(0) => {
                "INSERT INTO rookery.layouts (user_id, name, value, revision) VALUES ($1, $2, $3, 1)
                 ON CONFLICT (user_id, name) DO NOTHING RETURNING revision"
            }
            Some(_) => {
                "UPDATE rookery.layouts SET value = $3, revision = revision + 1, updated_at = now()
                 WHERE user_id = $1 AND name = $2 AND revision = $4 RETURNING revision"
            }
        };
        let found: Option<i64> = sqlx::query_scalar(sql)
            .bind(user)
            .bind(name)
            .bind(value)
            .bind(expected)
            .fetch_optional(self.pool)
            .await?;
        found.ok_or_else(|| match expected {
            Some(0) => Error::Conflict("the layout exists already".into()),
            other => Error::Conflict(format!(
                "the layout is not at revision {}",
                other.unwrap_or_default()
            )),
        })
    }
}
