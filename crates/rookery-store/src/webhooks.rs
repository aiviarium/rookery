//! Webhook deliveries: every accepted request to `/hooks/<plugin>/<name>` with the job it enqueued, and
//! the refused ones counted per minute. Replays are recognised by a key, a hash of what the delivery's
//! signature covers: a second delivery with the key of an accepted one enqueues nothing and points at the
//! first job, for as long as the accepted delivery is kept.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::Store;
use crate::error::Result;
use crate::jobs::{Job, NewJob};
use crate::util::new_id;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Delivery {
    pub id: Uuid,
    pub plugin: String,
    pub hook: String,
    /// `accepted`, `duplicate` or `refused`.
    pub outcome: String,
    /// The HTTP status the server answered with.
    pub status: i32,
    pub reason: Option<String>,
    pub replay_key: Option<String>,
    pub job_id: Option<Uuid>,
    pub bytes: i32,
    pub received_at: DateTime<Utc>,
}

/// The requests to a webhook refused in one minute with one status.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Refusals {
    pub plugin: String,
    pub hook: String,
    pub minute: DateTime<Utc>,
    pub status: i32,
    pub count: i64,
    pub reason: String,
    pub bytes: i32,
}

/// What [`Webhooks::accept`] did.
#[derive(Debug, Clone)]
pub enum Accepted {
    /// A new delivery; the job was enqueued.
    New { delivery: Delivery, job: Box<Job> },
    /// A replay of an accepted delivery; nothing was enqueued or recorded. `job` is the first delivery's
    /// job.
    Duplicate { job: Option<Uuid> },
}

pub struct Webhooks<'a> {
    pub(crate) store: &'a Store,
}

impl<'a> Webhooks<'a> {
    /// Records an accepted delivery and enqueues its job in one transaction, unless a delivery
    /// with the same replay key was accepted before. A replay is counted with the refusals, by the caller.
    pub async fn accept(
        &self,
        plugin: &str,
        hook: &str,
        replay_key: Option<&str>,
        bytes: usize,
        job: &NewJob<'_>,
    ) -> Result<Accepted> {
        let mut tx = self.store.pool().begin().await?;
        let id = new_id();
        let inserted: Option<Delivery> = sqlx::query_as(
            "INSERT INTO rookery.webhook_deliveries (id, plugin, hook, outcome, status, replay_key, bytes) \
             VALUES ($1, $2, $3, 'accepted', 202, $4, $5) \
             ON CONFLICT (plugin, hook, replay_key) WHERE replay_key IS NOT NULL AND outcome = 'accepted' DO NOTHING \
             RETURNING *",
        )
        .bind(id)
        .bind(plugin)
        .bind(hook)
        .bind(replay_key)
        .bind(bytes as i32)
        .fetch_optional(&mut *tx)
        .await?;
        match inserted {
            Some(_) => {
                let j = crate::jobs::enqueue_on(&mut tx, job, self.store.fence()).await?;
                let delivery: Delivery = sqlx::query_as(
                    "UPDATE rookery.webhook_deliveries SET job_id = $2 WHERE id = $1 RETURNING *",
                )
                .bind(id)
                .bind(j.id)
                .fetch_one(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(Accepted::New {
                    delivery,
                    job: Box::new(j),
                })
            }
            None => {
                let first: Option<Uuid> = sqlx::query_scalar(
                    "SELECT job_id FROM rookery.webhook_deliveries WHERE plugin = $1 AND hook = $2 AND replay_key = $3 \
                     AND outcome = 'accepted'",
                )
                .bind(plugin)
                .bind(hook)
                .bind(replay_key)
                .fetch_optional(&mut *tx)
                .await?
                .flatten();
                tx.commit().await?;
                Ok(Accepted::Duplicate { job: first })
            }
        }
    }

    /// Records how many requests to a webhook were refused in a minute with a status: `count` is the
    /// minute's count so far, and replaces a smaller one.
    pub async fn refused(&self, r: &Refusals) -> Result<()> {
        sqlx::query(
            "INSERT INTO rookery.webhook_refusals (plugin, hook, minute, status, count, reason, bytes) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (plugin, hook, minute, status) DO UPDATE SET \
                count = GREATEST(rookery.webhook_refusals.count, EXCLUDED.count), \
                reason = EXCLUDED.reason, bytes = EXCLUDED.bytes",
        )
        .bind(&r.plugin)
        .bind(&r.hook)
        .bind(r.minute)
        .bind(r.status)
        .bind(r.count)
        .bind(&r.reason)
        .bind(r.bytes)
        .execute(self.store.pool())
        .await?;
        Ok(())
    }

    /// The refusals of a webhook, newest minute first.
    pub async fn refusals(&self, plugin: &str, hook: &str, limit: i64) -> Result<Vec<Refusals>> {
        Ok(sqlx::query_as(
            "SELECT * FROM rookery.webhook_refusals WHERE plugin = $1 AND hook = $2 \
             ORDER BY minute DESC, status LIMIT $3",
        )
        .bind(plugin)
        .bind(hook)
        .bind(limit.clamp(1, 1000))
        .fetch_all(self.store.pool())
        .await?)
    }

    /// Newest first; of one plugin or hook when given.
    pub async fn list(
        &self,
        plugin: Option<&str>,
        hook: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Delivery>> {
        Ok(sqlx::query_as(
            "SELECT * FROM rookery.webhook_deliveries WHERE ($1::text IS NULL OR plugin = $1) AND ($2::text IS NULL OR hook = $2) \
             ORDER BY id DESC LIMIT $3",
        )
        .bind(plugin)
        .bind(hook)
        .bind(limit.clamp(1, 1000))
        .fetch_all(self.store.pool())
        .await?)
    }

    /// Deletes deliveries and refusals from before `before`. An accepted delivery, whose key refuses its
    /// replays, is kept for `keys` at least.
    pub async fn purge(&self, before: DateTime<Utc>, keys: chrono::Duration) -> Result<u64> {
        let deliveries = sqlx::query(
            "DELETE FROM rookery.webhook_deliveries WHERE received_at < $1 \
             AND (outcome <> 'accepted' OR replay_key IS NULL OR received_at < now() - $2::interval)",
        )
        .bind(before)
        .bind(format!("{} seconds", keys.num_seconds()))
        .execute(self.store.pool())
        .await?
        .rows_affected();
        let refusals = sqlx::query("DELETE FROM rookery.webhook_refusals WHERE minute < $1")
            .bind(before)
            .execute(self.store.pool())
            .await?
            .rows_affected();
        Ok(deliveries + refusals)
    }
}
