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
    /// A caller's token: a job of the same kind, subject and `params.action` with it already is
    /// returned in place of a new one.
    pub token: Option<&'a str>,
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

pub(crate) async fn queued_on(
    conn: &mut sqlx::PgConnection,
    kind: &str,
    at_most: i64,
) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "SELECT count(*) FROM (SELECT 1 FROM rookery.jobs WHERE state = 'queued' AND kind = $1 LIMIT $2) q",
    )
    .bind(kind)
    .bind(at_most)
    .fetch_one(conn)
    .await?)
}

/// Enqueues a job on a connection of the caller's, such as in the transaction of a batch of writes.
pub(crate) async fn enqueue_on(
    conn: &mut sqlx::PgConnection,
    job: &NewJob<'_>,
    fence: Option<i64>,
) -> Result<Job> {
    if !(1..=100).contains(&job.max_attempts) {
        return Err(Error::Invalid("maxAttempts is 1 to 100".into()));
    }
    let made: Option<Job> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO rookery.jobs (id, kind, params, state, max_attempts, run_after, subject, enqueued_by, token)
         SELECT $1, $2, $3, 'queued', $4, coalesce($5, now()), $6, $7, $8 WHERE rookery.fence($9)
         ON CONFLICT DO NOTHING RETURNING {COLS}"
    )))
    .bind(new_id())
    .bind(job.kind)
    .bind(job.params)
    .bind(job.max_attempts)
    .bind(job.run_after)
    .bind(job.subject)
    .bind(job.enqueued_by)
    .bind(job.token)
    .bind(fence)
    .fetch_optional(&mut *conn)
    .await?;
    match (made, job.token) {
        (Some(made), _) => Ok(made),
        (None, Some(token)) => by_token_on(
            conn,
            job.kind,
            job.subject,
            job.params["action"].as_str(),
            token,
        )
        .await?
        .ok_or_else(|| Error::Conflict("a job with the token was removed meanwhile".into())),
        (None, None) => Err(Error::Conflict("the job was not made".into())),
    }
}

/// The job a caller's token started, of a kind, subject and `params.action`.
async fn by_token_on(
    conn: &mut sqlx::PgConnection,
    kind: &str,
    subject: Option<&str>,
    action: Option<&str>,
    token: &str,
) -> Result<Option<Job>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM rookery.jobs WHERE kind = $1 AND subject IS NOT DISTINCT FROM $2
           AND params->>'action' IS NOT DISTINCT FROM $3 AND token = $4"
    )))
    .bind(kind)
    .bind(subject)
    .bind(action)
    .bind(token)
    .fetch_optional(conn)
    .await?)
}

pub struct Jobs<'a> {
    pub(crate) pool: &'a PgPool,
    /// The leader's epoch every write names: see [`crate::leader`].
    pub(crate) fence: Option<i64>,
}

impl Jobs<'_> {
    /// How many jobs of a kind are queued, counting to `at_most`.
    pub async fn queued(&self, kind: &str, at_most: i64) -> Result<i64> {
        let mut conn = self.pool.acquire().await?;
        queued_on(&mut conn, kind, at_most).await
    }

    pub async fn enqueue(&self, job: NewJob<'_>) -> Result<Job> {
        let mut conn = self.pool.acquire().await?;
        enqueue_on(&mut conn, &job, self.fence).await
    }

    /// The job a caller's token started, of a kind, subject and `params.action`.
    pub async fn by_token(
        &self,
        kind: &str,
        subject: Option<&str>,
        action: Option<&str>,
        token: &str,
    ) -> Result<Option<Job>> {
        let mut conn = self.pool.acquire().await?;
        by_token_on(&mut conn, kind, subject, action, token).await
    }

    /// The session on a host a job follows, and how far its output was copied.
    pub async fn set_session(&self, id: Uuid, session: &str, offset: i64) -> Result<()> {
        sqlx::query("UPDATE rookery.jobs SET session = $2, session_offset = $3 WHERE id = $1 AND rookery.fence($4)")
            .bind(id)
            .bind(session)
            .bind(offset)
            .bind(self.fence)
            .execute(self.pool)
            .await?;
        Ok(())
    }

    /// The session a job follows, and how far its log has it: the offset of the last log item that
    /// names one, which was written with the lines it covers, or where the session started.
    pub async fn session(&self, id: Uuid) -> Result<Option<(String, i64)>> {
        let row: Option<(Option<String>, Option<i64>)> = sqlx::query_as(
            "SELECT j.session, COALESCE(
                 (SELECT (l.data->>'session_offset')::bigint FROM rookery.log_items l
                   WHERE l.job_id = j.id AND l.data ? 'session_offset'
                   ORDER BY l.seq DESC LIMIT 1),
                 j.session_offset)
               FROM rookery.jobs j WHERE j.id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool)
        .await?;
        Ok(row.and_then(|(session, offset)| Some((session?, offset.unwrap_or(0)))))
    }

    /// Keeps the values a job's output masks, sealed, for a leader that follows the job on.
    pub async fn set_hidden(&self, id: Uuid, sealed: &crate::seal::Sealed) -> Result<()> {
        sqlx::query(
            "UPDATE rookery.jobs SET hidden_key_id = $2, hidden_nonce = $3, hidden = $4
              WHERE id = $1 AND rookery.fence($5)",
        )
        .bind(id)
        .bind(&sealed.key_id)
        .bind(&sealed.nonce)
        .bind(&sealed.ciphertext)
        .bind(self.fence)
        .execute(self.pool)
        .await?;
        Ok(())
    }

    pub async fn hidden(&self, id: Uuid) -> Result<Option<crate::seal::Sealed>> {
        type Kept = (Option<String>, Option<Vec<u8>>, Option<Vec<u8>>);
        let row: Option<Kept> = sqlx::query_as(
            "SELECT hidden_key_id, hidden_nonce, hidden FROM rookery.jobs WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool)
        .await?;
        Ok(row.and_then(|(key_id, nonce, ciphertext)| {
            Some(crate::seal::Sealed {
                key_id: key_id?,
                nonce: nonce?,
                ciphertext: ciphertext?,
            })
        }))
    }

    /// Moves the jobs of one subject to another, as when a host is renamed.
    pub async fn move_subject(&self, from: &str, to: &str) -> Result<u64> {
        let done = sqlx::query("UPDATE rookery.jobs SET subject = $2 WHERE subject = $1")
            .bind(from)
            .bind(to)
            .execute(self.pool)
            .await?;
        Ok(done.rows_affected())
    }

    /// Removes the jobs that ended before `cutoff`, with their logs.
    pub async fn prune(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        let done = sqlx::query("DELETE FROM rookery.jobs WHERE finished_at < $1")
            .bind(cutoff)
            .execute(self.pool)
            .await?;
        Ok(done.rows_affected())
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
               AND rookery.fence($4)
             RETURNING {COLS}"
        )))
        .bind(kinds)
        .bind(worker)
        .bind(lease.as_secs_f64())
        .bind(self.fence)
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
             WHERE id = $1 AND state = 'running' AND claimed_by = $2 AND rookery.fence($4) RETURNING cancel_requested",
        )
        .bind(id)
        .bind(worker)
        .bind(lease.as_secs_f64())
        .bind(self.fence)
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
             WHERE id = $1 AND state = 'running' AND claimed_by = $2 AND rookery.fence($7) RETURNING {COLS}"
        )))
        .bind(id)
        .bind(worker)
        .bind(state)
        .bind(result)
        .bind(error)
        .bind(retry_in)
        .bind(self.fence)
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
             WHERE id = $1 AND rookery.fence($2) RETURNING {COLS}"
        )))
        .bind(id)
        .bind(self.fence)
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
             WHERE id = $1 AND state IN ('failed', 'cancelled') AND rookery.fence($2) RETURNING {COLS}"
        )))
        .bind(id)
        .bind(self.fence)
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
             AND ($3::text IS NULL OR claimed_by = $3) AND rookery.fence($4)",
        )
        .bind(id)
        .bind(progress)
        .bind(worker)
        .bind(self.fence)
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
        // One statement, so no lock on the job's row outlives it: the UPDATE takes the next number
        // of the job's log and waits for an append before it, whose number it then sees.
        let seq: Option<i64> = sqlx::query_scalar(
            "WITH job AS (
                 UPDATE rookery.jobs SET log_seq = log_seq + 1
                  WHERE id = $1
                    AND ($2::text IS NULL OR (state = 'running' AND claimed_by = $2))
                    AND rookery.fence($3)
                 RETURNING id, attempts, log_seq)
             INSERT INTO rookery.log_items (job_id, seq, attempt, level, text, data)
             SELECT id, log_seq, attempts, $4, $5, $6 FROM job
             RETURNING seq",
        )
        .bind(id)
        .bind(worker)
        .bind(self.fence)
        .bind(level)
        .bind(text)
        .bind(data)
        .fetch_optional(self.pool)
        .await?;
        let seq = seq.ok_or_else(|| match worker {
            Some(_) => Error::Conflict("the job's claim is gone".into()),
            None => Error::NotFound("no such job".into()),
        })?;
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
        self.end_claims(kinds, once, Ok(worker), "the server running it stopped")
            .await
    }

    /// Ends the attempts of `kinds` whose claim ran out, as their worker stopped renewing it, but those
    /// in `running`, which this server runs and renews itself: a job of a kind in `once` fails as
    /// interrupted, another runs again while it has attempts left.
    pub async fn reclaim_expired(
        &self,
        kinds: &[String],
        once: &[String],
        running: &[Uuid],
    ) -> Result<Vec<Job>> {
        self.end_claims(
            kinds,
            once,
            Err(running),
            "the attempt was interrupted: its claim ran out",
        )
        .await
    }

    /// Ends running attempts: those of other workers than `Ok(worker)`, or those whose claim ran out but
    /// the jobs in `Err(running)`.
    async fn end_claims(
        &self,
        kinds: &[String],
        once: &[String],
        which: std::result::Result<&str, &[Uuid]>,
        why: &str,
    ) -> Result<Vec<Job>> {
        let (worker, running) = match which {
            Ok(worker) => (Some(worker), &[][..]),
            Err(running) => (None, running),
        };
        let error = serde_json::json!({ "code": "UNAVAILABLE", "message": why });
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE rookery.jobs SET
                 state = CASE WHEN kind = ANY ($2) OR attempts >= max_attempts OR cancel_requested THEN
                              CASE WHEN cancel_requested THEN 'cancelled' ELSE 'failed' END ELSE 'queued' END,
                 error = CASE WHEN cancel_requested THEN error ELSE $4 END,
                 finished_at = CASE WHEN kind = ANY ($2) OR attempts >= max_attempts OR cancel_requested THEN now() END,
                 run_after = least(run_after, now()),
                 claimed_by = NULL, claimed_until = NULL
             WHERE state = 'running' AND kind = ANY ($1)
               AND CASE WHEN $3::text IS NULL THEN claimed_until < now() AND id <> ALL ($6)
                        ELSE claimed_by IS DISTINCT FROM $3 END
               AND rookery.fence($5)
             RETURNING {COLS}"
        )))
        .bind(kinds)
        .bind(once)
        .bind(worker)
        .bind(error)
        .bind(self.fence)
        .bind(running)
        .fetch_all(self.pool)
        .await?)
    }
}
