//! What a user opened and ran last, kept per user so it follows them to every device: the palette's
//! and the home screen's "recent" lists, and what an empty search offers first. Entities are listed
//! only while the caller may still view them.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::Store;
use crate::caller::Caller;
use crate::entities::{ENTITY_COLUMNS, Entity, visible_sql};
use crate::error::Result;

/// How many entries of each kind are kept per user; older ones are dropped as new ones arrive.
pub const KEEP: i64 = 200;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct RecentCommand {
    pub id: String,
    pub opened_at: DateTime<Utc>,
    pub count: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentEntity {
    pub entity: Entity,
    pub opened_at: DateTime<Utc>,
}

pub struct Recents<'a> {
    pub(crate) store: &'a Store,
}

impl<'a> Recents<'a> {
    /// Notes that the user opened an entity. Returns whether it was kept: an entity the caller may not
    /// view is not recorded, and the caller is not told whether it exists.
    pub async fn touch_entity(&self, caller: &Caller, uri: &str) -> Result<bool> {
        let user = caller.require_user()?;
        if self.store.entities().get(caller, uri).await?.is_none() {
            return Ok(false);
        }
        self.touch(user, "entity", uri).await?;
        Ok(true)
    }

    /// Notes that the user ran a command.
    pub async fn touch_command(&self, caller: &Caller, id: &str) -> Result<()> {
        let user = caller.require_user()?;
        self.touch(user, "command", id).await
    }

    async fn touch(&self, user: Uuid, kind: &str, key: &str) -> Result<()> {
        let mut tx = self.store.pool().begin().await?;
        sqlx::query(
            "INSERT INTO rookery.recents (user_id, kind, key) VALUES ($1, $2, $3) \
             ON CONFLICT (user_id, kind, key) DO UPDATE \
             SET opened_at = now(), count = rookery.recents.count + 1",
        )
        .bind(user)
        .bind(kind)
        .bind(key)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM rookery.recents WHERE user_id = $1 AND kind = $2 AND key IN \
             (SELECT key FROM rookery.recents WHERE user_id = $1 AND kind = $2 \
              ORDER BY opened_at DESC OFFSET $3)",
        )
        .bind(user)
        .bind(kind)
        .bind(KEEP)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Forgets one entry, or everything of a kind when `key` is `None`.
    pub async fn forget(&self, caller: &Caller, kind: &str, key: Option<&str>) -> Result<u64> {
        let user = caller.require_user()?;
        Ok(sqlx::query(
            "DELETE FROM rookery.recents WHERE user_id = $1 AND kind = $2 AND ($3::text IS NULL OR key = $3)",
        )
        .bind(user)
        .bind(kind)
        .bind(key)
        .execute(self.store.pool())
        .await?
        .rows_affected())
    }

    /// The commands the user ran last, newest first.
    pub async fn commands(&self, caller: &Caller, limit: i64) -> Result<Vec<RecentCommand>> {
        let user = caller.require_user()?;
        Ok(sqlx::query_as(
            "SELECT key AS id, opened_at, count FROM rookery.recents \
             WHERE user_id = $1 AND kind = 'command' ORDER BY opened_at DESC LIMIT $2",
        )
        .bind(user)
        .bind(limit.clamp(1, KEEP))
        .fetch_all(self.store.pool())
        .await?)
    }

    /// The entities the user opened last that they may still view, newest first.
    pub async fn entities(
        &self,
        caller: &Caller,
        schemes: Option<&[String]>,
        space: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<RecentEntity>> {
        let user = caller.require_user()?;
        let sql = format!(
            "SELECT {ENTITY_COLUMNS}, r.opened_at AS opened_at \
             FROM rookery.recents r JOIN rookery.entities e ON e.uri = r.key \
             WHERE r.user_id = $1 AND r.kind = 'entity' AND e.deleted_at IS NULL \
               AND ($4::text[] IS NULL OR e.scheme = ANY($4)) \
               AND ($5::uuid IS NULL OR e.space_id = $5) \
               AND {} \
             ORDER BY r.opened_at DESC LIMIT $6",
            visible_sql("$1", "$2", "$3")
        );
        let rows: Vec<RecentRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql.to_string()))
            .bind(user)
            .bind(caller.actor())
            .bind(caller.admin)
            .bind(schemes)
            .bind(space)
            .bind(limit.clamp(1, KEEP))
            .fetch_all(self.store.pool())
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| RecentEntity {
                entity: r.entity,
                opened_at: r.opened_at,
            })
            .collect())
    }
}

#[derive(sqlx::FromRow)]
struct RecentRow {
    #[sqlx(flatten)]
    entity: Entity,
    opened_at: DateTime<Utc>,
}
