//! Webhook deliveries: every request to `/hooks/<plugin>/<name>`, accepted or refused, with the job
//! an accepted one enqueued. Replays are recognised by a key (the provider's delivery id, or the
//! signature for schemes that sign a timestamp): a second delivery with the key of an accepted one
//! enqueues nothing and points at the first job.

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

/// What [`Webhooks::accept`] did.
#[derive(Debug, Clone)]
pub enum Accepted {
    /// A new delivery; the job was enqueued.
    New { delivery: Delivery, job: Box<Job> },
    /// A replay of an accepted delivery; nothing was enqueued. `job` is the first delivery's job.
    Duplicate {
        delivery: Delivery,
        job: Option<Uuid>,
    },
}

pub struct Webhooks<'a> {
    pub(crate) store: &'a Store,
}

impl<'a> Webhooks<'a> {
    /// Records an accepted delivery and enqueues its job in one transaction, unless a delivery
    /// with the same replay key was accepted before.
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
                let delivery: Delivery = sqlx::query_as(
                    "INSERT INTO rookery.webhook_deliveries (id, plugin, hook, outcome, status, reason, replay_key, job_id, bytes) \
                     VALUES ($1, $2, $3, 'duplicate', 200, 'a delivery with this id was accepted before', $4, $5, $6) \
                     RETURNING *",
                )
                .bind(id)
                .bind(plugin)
                .bind(hook)
                .bind(replay_key)
                .bind(first)
                .bind(bytes as i32)
                .fetch_one(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(Accepted::Duplicate {
                    delivery,
                    job: first,
                })
            }
        }
    }

    /// Records a refused delivery.
    pub async fn refuse(
        &self,
        plugin: &str,
        hook: &str,
        status: u16,
        reason: &str,
        bytes: usize,
    ) -> Result<Delivery> {
        Ok(sqlx::query_as(
            "INSERT INTO rookery.webhook_deliveries (id, plugin, hook, outcome, status, reason, bytes) \
             VALUES ($1, $2, $3, 'refused', $4, $5, $6) RETURNING *",
        )
        .bind(new_id())
        .bind(plugin)
        .bind(hook)
        .bind(status as i32)
        .bind(reason)
        .bind(bytes as i32)
        .fetch_one(self.store.pool())
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

    /// Deletes deliveries received before `before`.
    pub async fn purge(&self, before: DateTime<Utc>) -> Result<u64> {
        Ok(
            sqlx::query("DELETE FROM rookery.webhook_deliveries WHERE received_at < $1")
                .bind(before)
                .execute(self.store.pool())
                .await?
                .rows_affected(),
        )
    }
}
