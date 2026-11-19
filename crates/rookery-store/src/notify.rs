//! Notifications: an inbox for each person. A notification can offer answers, such as approve or
//! deny, and keeps the one given. A person can mute a kind.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::util::new_id;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Notification {
    pub id: Uuid,
    pub user_id: Uuid,
    pub kind: String,
    pub title: String,
    pub body: Option<String>,
    pub data: Option<Value>,
    pub actions: Option<Value>,
    pub answer: Option<String>,
    pub sent_by: String,
    pub created_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
    pub answered_at: Option<DateTime<Utc>>,
}

const COLS: &str = "id, user_id, kind, title, body, data, actions, answer, sent_by, created_at, read_at, answered_at";

#[derive(Debug, Clone)]
pub struct New<'a> {
    pub user: Uuid,
    pub kind: &'a str,
    pub title: &'a str,
    pub body: Option<&'a str>,
    pub data: Option<&'a Value>,
    pub actions: Option<&'a [String]>,
    pub sent_by: &'a str,
}

pub struct Notify<'a> {
    pub(crate) pool: &'a PgPool,
}

impl Notify<'_> {
    /// Puts a notification in a person's inbox; `None` when they muted its kind.
    pub async fn send(&self, new: New<'_>) -> Result<Option<Notification>> {
        let muted: Option<bool> = sqlx::query_scalar(
            "SELECT muted FROM rookery.notification_prefs WHERE user_id = $1 AND kind = $2",
        )
        .bind(new.user)
        .bind(new.kind)
        .fetch_optional(self.pool)
        .await?;
        if muted == Some(true) {
            return Ok(None);
        }
        let made = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "INSERT INTO rookery.notifications (id, user_id, kind, title, body, data, actions, sent_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {COLS}"
        )))
        .bind(new_id())
        .bind(new.user)
        .bind(new.kind)
        .bind(new.title)
        .bind(new.body)
        .bind(new.data)
        .bind(new.actions.map(|a| serde_json::json!(a)))
        .bind(new.sent_by)
        .fetch_one(self.pool)
        .await;
        match made {
            Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23503") => {
                Err(Error::NotFound("no such user".into()))
            }
            other => Ok(Some(other?)),
        }
    }

    /// The newest first; with `unread`, only those not read.
    pub async fn list(
        &self,
        user: Uuid,
        unread: bool,
        before: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<Notification>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLS} FROM rookery.notifications WHERE user_id = $1 AND (NOT $2 OR read_at IS NULL) AND ($3::uuid IS NULL OR id < $3)
             ORDER BY id DESC LIMIT $4"
        )))
        .bind(user)
        .bind(unread)
        .bind(before)
        .bind(limit.clamp(1, 500))
        .fetch_all(self.pool)
        .await?)
    }

    pub async fn unread(&self, user: Uuid) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT count(*) FROM rookery.notifications WHERE user_id = $1 AND read_at IS NULL",
        )
        .bind(user)
        .fetch_one(self.pool)
        .await?)
    }

    /// Marks some, or with no ids all, of a person's notifications read. Returns how many changed.
    pub async fn read(&self, user: Uuid, ids: Option<&[Uuid]>) -> Result<u64> {
        let done = sqlx::query(
            "UPDATE rookery.notifications SET read_at = now() WHERE user_id = $1 AND read_at IS NULL AND ($2::uuid[] IS NULL OR id = ANY ($2))",
        )
        .bind(user)
        .bind(ids)
        .execute(self.pool)
        .await?;
        Ok(done.rows_affected())
    }

    /// Gives one of a notification's answers, once.
    pub async fn answer(&self, user: Uuid, id: Uuid, answer: &str) -> Result<Notification> {
        let found: Option<Notification> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLS} FROM rookery.notifications WHERE id = $1 AND user_id = $2"
        )))
        .bind(id)
        .bind(user)
        .fetch_optional(self.pool)
        .await?;
        let Some(found) = found else {
            return Err(Error::NotFound("no such notification".into()));
        };
        let offered = found
            .actions
            .as_ref()
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(answer)));
        if !offered {
            return Err(Error::Invalid(format!(
                "{answer:?} is not one of the notification's answers"
            )));
        }
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE rookery.notifications SET answer = $3, answered_at = now(), read_at = coalesce(read_at, now())
             WHERE id = $1 AND user_id = $2 AND answer IS NULL RETURNING {COLS}"
        )))
        .bind(id)
        .bind(user)
        .bind(answer)
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| Error::Conflict("the notification was answered already".into()))
    }

    pub async fn set_pref(&self, user: Uuid, kind: &str, muted: bool) -> Result<()> {
        sqlx::query(
            "INSERT INTO rookery.notification_prefs (user_id, kind, muted) VALUES ($1, $2, $3)
             ON CONFLICT (user_id, kind) DO UPDATE SET muted = EXCLUDED.muted",
        )
        .bind(user)
        .bind(kind)
        .bind(muted)
        .execute(self.pool)
        .await?;
        Ok(())
    }

    pub async fn prefs(&self, user: Uuid) -> Result<Vec<(String, bool)>> {
        Ok(sqlx::query_as(
            "SELECT kind, muted FROM rookery.notification_prefs WHERE user_id = $1 ORDER BY kind",
        )
        .bind(user)
        .fetch_all(self.pool)
        .await?)
    }
}
