//! Jobs: records, claims, retries, cancelling, progress and the log.
//!
//! A worker claims a queued job of a kind it handles and holds the claim while it runs, renewing it.
//! Cancelling a queued job ends it; a running one gets `cancel_requested`, which its handler sees. A
//! failed attempt is tried again while attempts are left; a job with one attempt never is.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::util::new_id;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Job {
    pub id: Uuid,
    pub kind: String,
    pub params: Value,
    pub state: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub run_after: DateTime<Utc>,
    pub claimed_by: Option<String>,
    pub cancel_requested: bool,
    pub progress: Option<Value>,
    pub result: Option<Value>,
    pub error: Option<Value>,
    pub subject: Option<String>,
    pub enqueued_by: String,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl Job {
    pub fn is_done(&self) -> bool {
        matches!(self.state.as_str(), "succeeded" | "failed" | "cancelled")
    }
}

const COLS: &str = "id, kind, params, state, attempts, max_attempts, run_after, claimed_by, cancel_requested, progress, result, error, subject, enqueued_by, created_at, started_at, finished_at";

#[derive(Debug, Clone)]
pub struct NewJob<'a> {
    pub kind: &'a str,
    pub params: &'a Value,
    pub max_attempts: i32,
    pub run_after: Option<DateTime<Utc>>,
    pub subject: Option<&'a str>,
    pub enqueued_by: &'a str,
}

#[derive(Debug, Clone, Default)]
pub struct Filter<'a> {
    pub enqueued_by: Option<&'a str>,
    pub state: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub subject: Option<&'a str>,
    /// Jobs made before this one, to page back.
    pub before: Option<Uuid>,
    pub limit: i64,
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct LogItem {
    pub seq: i64,
    pub at: DateTime<Utc>,
    pub attempt: i32,
    pub level: String,
    pub text: String,
    pub data: Option<Value>,
}

/// How an attempt ended.
#[derive(Debug, Clone)]
pub enum Outcome {
    Succeeded(Value),
    Failed {
        error: Value,
        retry_in: Option<std::time::Duration>,
    },
    Cancelled,
}

pub struct Jobs<'a> {
    pub(crate) pool: &'a PgPool,
}

impl Jobs<'_> {
    pub async fn enqueue(&self, job: NewJob<'_>) -> Result<Job> {
        if !(1..=100).contains(&job.max_attempts) {
            return Err(Error::Invalid("maxAttempts is 1 to 100".into()));
        }
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "INSERT INTO rookery.jobs (id, kind, params, state, max_attempts, run_after, subject, enqueued_by)
             VALUES ($1, $2, $3, 'queued', $4, coalesce($5, now()), $6, $7) RETURNING {COLS}"
        )))
        .bind(new_id())
        .bind(job.kind)
        .bind(job.params)
        .bind(job.max_attempts)
        .bind(job.run_after)
        .bind(job.subject)
        .bind(job.enqueued_by)
        .fetch_one(self.pool)
        .await?)
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<Job>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLS} FROM rookery.jobs WHERE id = $1"
        )))
        .bind(id)
        .fetch_optional(self.pool)
        .await?)
    }

    /// The newest first.
    pub async fn list(&self, filter: &Filter<'_>) -> Result<Vec<Job>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLS} FROM rookery.jobs
             WHERE ($1::text IS NULL OR enqueued_by = $1) AND ($2::text IS NULL OR state = $2) AND ($3::text IS NULL OR kind = $3)
               AND ($4::text IS NULL OR subject = $4) AND ($5::uuid IS NULL OR id < $5)
             ORDER BY id DESC LIMIT $6"
        )))
        .bind(filter.enqueued_by)
        .bind(filter.state)
        .bind(filter.kind)
        .bind(filter.subject)
        .bind(filter.before)
        .bind(filter.limit.clamp(1, 500))
        .fetch_all(self.pool)
        .await?)
    }

    /// Claims the oldest ready job of one of `kinds` for `worker`, for `lease`.
    pub async fn claim(
        &self,
        kinds: &[String],
        worker: &str,
        lease: std::time::Duration,
    ) -> Result<Option<Job>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE rookery.jobs SET state = 'running', attempts = attempts + 1, claimed_by = $2,
                 claimed_until = now() + make_interval(secs => $3), started_at = coalesce(started_at, now())
             WHERE id = (SELECT id FROM rookery.jobs WHERE state = 'queued' AND kind = ANY ($1) AND run_after <= now()
                         ORDER BY run_after, id LIMIT 1 FOR UPDATE SKIP LOCKED)
             RETURNING {COLS}"
        )))
        .bind(kinds)
        .bind(worker)
        .bind(lease.as_secs_f64())
        .fetch_optional(self.pool)
        .await?)
    }

    /// When the next queued job of `kinds` is ready, if one is waiting.
    pub async fn next_ready(&self, kinds: &[String]) -> Result<Option<DateTime<Utc>>> {
        Ok(sqlx::query_scalar(
            "SELECT min(run_after) FROM rookery.jobs WHERE state = 'queued' AND kind = ANY ($1)",
        )
        .bind(kinds)
        .fetch_one(self.pool)
        .await?)
    }

    /// Keeps a claim. Returns whether the job was asked to stop, or an error when the claim is gone.
    pub async fn heartbeat(
        &self,
        id: Uuid,
        worker: &str,
        lease: std::time::Duration,
    ) -> Result<bool> {
        sqlx::query_scalar(
            "UPDATE rookery.jobs SET claimed_until = now() + make_interval(secs => $3)
             WHERE id = $1 AND state = 'running' AND claimed_by = $2 RETURNING cancel_requested",
        )
        .bind(id)
        .bind(worker)
        .bind(lease.as_secs_f64())
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| Error::Conflict("the job's claim is gone".into()))
    }

    /// Ends an attempt of a claimed job.
    pub async fn finish(&self, id: Uuid, worker: &str, outcome: Outcome) -> Result<Job> {
        let (state, result, error, retry_in) = match outcome {
            Outcome::Succeeded(result) => ("succeeded", Some(result), None, None),
            Outcome::Cancelled => ("cancelled", None, None, None),
            Outcome::Failed { error, retry_in } => (
                "failed",
                None,
                Some(error),
                retry_in.map(|duration| duration.as_secs_f64()),
            ),
        };
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE rookery.jobs SET
                 state = CASE WHEN cancel_requested THEN 'cancelled'
                              WHEN attempts < max_attempts AND $6::float8 IS NOT NULL THEN 'queued'
                              ELSE $3 END,
                 result = CASE WHEN NOT cancel_requested THEN $4 END,
                 error = CASE WHEN NOT cancel_requested THEN $5 END,
                 run_after = CASE WHEN NOT cancel_requested AND attempts < max_attempts AND $6::float8 IS NOT NULL
                                  THEN now() + make_interval(secs => $6) ELSE run_after END,
                 finished_at = CASE WHEN NOT cancel_requested AND attempts < max_attempts AND $6::float8 IS NOT NULL
                                    THEN NULL ELSE now() END,
                 claimed_by = NULL, claimed_until = NULL
             WHERE id = $1 AND state = 'running' AND claimed_by = $2 RETURNING {COLS}"
        )))
        .bind(id)
        .bind(worker)
        .bind(state)
        .bind(result)
        .bind(error)
        .bind(retry_in)
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| Error::Conflict("the job's claim is gone".into()))
    }

    /// Cancels a job: a queued one ends now, a running one is asked to stop.
    pub async fn cancel(&self, id: Uuid) -> Result<Job> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE rookery.jobs SET
                 cancel_requested = true,
                 state = CASE WHEN state = 'queued' THEN 'cancelled' ELSE state END,
                 finished_at = CASE WHEN state = 'queued' THEN now() ELSE finished_at END
             WHERE id = $1 RETURNING {COLS}"
        )))
        .bind(id)
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| Error::NotFound("no such job".into()))
    }

    /// Runs a failed or cancelled job again, with one more attempt.
    pub async fn retry(&self, id: Uuid) -> Result<Job> {
        let job = self
            .get(id)
            .await?
            .ok_or_else(|| Error::NotFound("no such job".into()))?;
        if !matches!(job.state.as_str(), "failed" | "cancelled") {
            return Err(Error::Conflict(format!(
                "the job is {}; only a failed or cancelled one runs again",
                job.state
            )));
        }
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE rookery.jobs SET state = 'queued', max_attempts = attempts + 1, cancel_requested = false, run_after = now(),
                 finished_at = NULL, error = NULL, result = NULL
             WHERE id = $1 AND state IN ('failed', 'cancelled') RETURNING {COLS}"
        )))
        .bind(id)
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| Error::Conflict("the job changed meanwhile".into()))
    }

    pub async fn set_progress(&self, id: Uuid, progress: &Value) -> Result<()> {
        self.update_progress(id, None, progress).await
    }

    pub async fn set_claimed_progress(
        &self,
        id: Uuid,
        worker: &str,
        progress: &Value,
    ) -> Result<()> {
        self.update_progress(id, Some(worker), progress).await
    }

    async fn update_progress(
        &self,
        id: Uuid,
        worker: Option<&str>,
        progress: &Value,
    ) -> Result<()> {
        let done = sqlx::query(
            "UPDATE rookery.jobs SET progress = $2 WHERE id = $1 AND state = 'running'
             AND ($3::text IS NULL OR claimed_by = $3)",
        )
        .bind(id)
        .bind(progress)
        .bind(worker)
        .execute(self.pool)
        .await?;
        if done.rows_affected() == 0 {
            return Err(Error::Conflict("the job is not running".into()));
        }
        Ok(())
    }

    /// Appends to a job's log and returns the item's place in it.
    pub async fn log(
        &self,
        id: Uuid,
        level: &str,
        text: &str,
        data: Option<&Value>,
    ) -> Result<i64> {
        self.append_log(id, None, level, text, data).await
    }

    pub async fn log_claimed(
        &self,
        id: Uuid,
        worker: &str,
        level: &str,
        text: &str,
        data: Option<&Value>,
    ) -> Result<i64> {
        self.append_log(id, Some(worker), level, text, data).await
    }

    async fn append_log(
        &self,
        id: Uuid,
        worker: Option<&str>,
        level: &str,
        text: &str,
        data: Option<&Value>,
    ) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        let attempt: i32 = sqlx::query_scalar(
            "SELECT attempts FROM rookery.jobs WHERE id = $1
               AND ($2::text IS NULL OR (state = 'running' AND claimed_by = $2))
             FOR UPDATE",
        )
        .bind(id)
        .bind(worker)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| match worker {
            Some(_) => Error::Conflict("the job's claim is gone".into()),
            None => Error::NotFound("no such job".into()),
        })?;
        // Compute the next sequence in a fresh snapshot after taking the job row lock.
        let seq = sqlx::query_scalar(
            "INSERT INTO rookery.log_items (job_id, seq, attempt, level, text, data)
             VALUES ($1, coalesce((SELECT max(seq) FROM rookery.log_items WHERE job_id = $1), 0) + 1, $2, $3, $4, $5)
             RETURNING seq",
        )
        .bind(id)
        .bind(attempt)
        .bind(level)
        .bind(text)
        .bind(data)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(seq)
    }

    /// A job's log items after `after`, in order.
    pub async fn log_after(&self, id: Uuid, after: i64, limit: i64) -> Result<Vec<LogItem>> {
        Ok(sqlx::query_as(
            "SELECT seq, at, attempt, level, text, data FROM rookery.log_items WHERE job_id = $1 AND seq > $2 ORDER BY seq LIMIT $3",
        )
        .bind(id)
        .bind(after)
        .bind(limit.clamp(1, 10_000))
        .fetch_all(self.pool)
        .await?)
    }

    /// Ends the attempts a server that stopped left running: of `kinds`, claimed by any other worker
    /// than `worker`. A job of a kind in `once` fails as interrupted; another runs again while it has
    /// attempts left.
    pub async fn recover(
        &self,
        kinds: &[String],
        once: &[String],
        worker: &str,
    ) -> Result<Vec<Job>> {
        let error = serde_json::json!({ "code": "UNAVAILABLE", "message": "the server running it stopped" });
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE rookery.jobs SET
                 state = CASE WHEN kind = ANY ($2) OR attempts >= max_attempts OR cancel_requested THEN
                              CASE WHEN cancel_requested THEN 'cancelled' ELSE 'failed' END ELSE 'queued' END,
                 error = CASE WHEN cancel_requested THEN error ELSE $4 END,
                 finished_at = CASE WHEN kind = ANY ($2) OR attempts >= max_attempts OR cancel_requested THEN now() END,
                 claimed_by = NULL, claimed_until = NULL
             WHERE state = 'running' AND kind = ANY ($1) AND claimed_by IS DISTINCT FROM $3
             RETURNING {COLS}"
        )))
        .bind(kinds)
        .bind(once)
        .bind(worker)
        .bind(error)
        .fetch_all(self.pool)
        .await?)
    }
}
