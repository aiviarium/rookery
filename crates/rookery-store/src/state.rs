//! The server's own small values, such as its deployment id.

use sqlx::PgPool;

use crate::error::Result;

pub struct State<'a> {
    pub(crate) pool: &'a PgPool,
}

impl State<'_> {
    pub async fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT value FROM rookery.server_state WHERE key = $1")
                .bind(key)
                .fetch_optional(self.pool)
                .await?,
        )
    }

    /// The value of `key`, set to `make()` the first time. Two servers asking at once get the same one.
    pub async fn get_or_init(&self, key: &str, make: impl FnOnce() -> String) -> Result<String> {
        sqlx::query("INSERT INTO rookery.server_state (key, value) VALUES ($1, $2) ON CONFLICT (key) DO NOTHING")
            .bind(key)
            .bind(make())
            .execute(self.pool)
            .await?;
        Ok(
            sqlx::query_scalar("SELECT value FROM rookery.server_state WHERE key = $1")
                .bind(key)
                .fetch_one(self.pool)
                .await?,
        )
    }

    /// The id of this installation, made once.
    pub async fn deployment_id(&self) -> Result<String> {
        self.get_or_init("deployment_id", || {
            crate::util::new_id().simple().to_string()
        })
        .await
    }
}
