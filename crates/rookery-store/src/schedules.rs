//! Schedules: cron expressions (5 fields, or 6 with seconds) in the schedule's time zone, or fixed
//! intervals, that enqueue jobs. The leader calls [`Schedules::enqueue_due`] every second.
//!
//! A schedule enqueues a job of a kind a handler runs: one of the server's own, such as `apply` with
//! its `subject` `env://NAME`, or a plugin's `<plugin>/<job>`. It is declared in its plugin's manifest
//! (synced with [`Schedules::sync_declared`] when the plugin loads), or made while the server runs. A
//! paused one (`enabled = false`) keeps its definition.
//!
//! Runs missed while no server ran follow the schedule's `missed` policy: `once` fires one job for all
//! of them, `skip` none and waits for the next run, `all` one per missed run (at most [`MAX_CATCH_UP`]).
//! A run that is due while the schedule's last job is still open starts no second one.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde_json::Value;
use uuid::Uuid;

use crate::Store;
use crate::error::{Error, Result};
use crate::jobs::{Job, NewJob};
use crate::util::new_id;

/// A run counts as missed, and the `missed` policy applies, when it is this late.
pub const MISSED_AFTER: chrono::Duration = chrono::Duration::seconds(90);
/// `missed: all` fires at most this many runs it caught up on.
pub const MAX_CATCH_UP: usize = 100;
/// Schedules a plugin has at most, declared and made.
pub const MAX_PER_PLUGIN: i64 = 100;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Schedule {
    pub id: Uuid,
    pub plugin: String,
    pub name: String,
    pub title: Option<String>,
    pub kind: String,
    pub params: Value,
    pub subject: Option<String>,
    pub cron: Option<String>,
    pub interval_secs: Option<i64>,
    pub timezone: String,
    /// `once`, `skip` or `all`.
    pub missed: String,
    pub principal: String,
    pub on_behalf_of: Option<Uuid>,
    pub space_id: Option<Uuid>,
    pub enabled: bool,
    /// Why the server paused it, if it did.
    pub paused_reason: Option<String>,
    pub declared: bool,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_job_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewSchedule {
    pub plugin: String,
    pub name: String,
    pub title: Option<String>,
    pub kind: String,
    pub params: Value,
    pub subject: Option<String>,
    pub cron: Option<String>,
    pub interval_secs: Option<i64>,
    /// An IANA time zone for cron expressions; UTC without one.
    pub timezone: Option<String>,
    /// `once` (without one), `skip` or `all`.
    pub missed: Option<String>,
    pub principal: String,
    pub on_behalf_of: Option<Uuid>,
    pub space_id: Option<Uuid>,
    pub enabled: bool,
}

impl NewSchedule {
    pub fn new(
        plugin: impl Into<String>,
        name: impl Into<String>,
        kind: impl Into<String>,
        principal: impl Into<String>,
    ) -> Self {
        NewSchedule {
            plugin: plugin.into(),
            name: name.into(),
            title: None,
            kind: kind.into(),
            params: Value::Object(Default::default()),
            subject: None,
            cron: None,
            interval_secs: None,
            timezone: None,
            missed: None,
            principal: principal.into(),
            on_behalf_of: None,
            space_id: None,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScheduleQuery {
    pub plugin: Option<String>,
    /// Those a plugin made for this person.
    pub user: Option<Uuid>,
    pub declared: Option<bool>,
}

/// An IANA time zone.
pub fn parse_tz(name: &str) -> Result<Tz> {
    name.parse::<Tz>().map_err(|_| {
        Error::invalid(format!(
            "{name:?} is no time zone: use an IANA name such as Europe/Berlin"
        ))
    })
}

fn check_missed(m: Option<&str>) -> Result<&'static str> {
    match m.unwrap_or("once") {
        "once" => Ok("once"),
        "skip" => Ok("skip"),
        "all" => Ok("all"),
        other => Err(Error::invalid(format!(
            "missed is once, skip or all, not {other:?}"
        ))),
    }
}

/// The next time after `after` that a cron expression, in `tz`, or an interval fires.
pub fn next_run_in(
    cron: Option<&str>,
    interval_secs: Option<i64>,
    tz: &str,
    after: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    match (cron, interval_secs) {
        (Some(expr), None) => {
            let c = croner::Cron::new(expr)
                .with_seconds_optional()
                .parse()
                .map_err(|e| Error::invalid(format!("{expr:?} is no cron expression: {e}")))?;
            let tz = parse_tz(tz)?;
            let local = after.with_timezone(&tz);
            c.find_next_occurrence(&local, false)
                .map(|t| t.with_timezone(&Utc))
                .map_err(|e| {
                    Error::invalid(format!("the cron expression {expr:?} never fires: {e}"))
                })
        }
        (None, Some(secs)) if secs > 0 => Ok(after + chrono::Duration::seconds(secs)),
        _ => Err(Error::invalid(
            "a schedule has a cron expression or an interval of at least a second",
        )),
    }
}

fn object(v: &Value) -> Value {
    if v.is_null() {
        Value::Object(Default::default())
    } else {
        v.clone()
    }
}

/// Enqueues a schedule's job on a connection of the caller's.
async fn enqueue(
    conn: &mut sqlx::PgConnection,
    s: &Schedule,
    params: &Value,
    fence: Option<i64>,
) -> Result<Job> {
    crate::jobs::enqueue_on(
        conn,
        &NewJob {
            kind: &s.kind,
            params,
            max_attempts: 1,
            run_after: None,
            subject: s.subject.as_deref(),
            enqueued_by: &s.principal,
            token: None,
        },
        fence,
    )
    .await
}

/// Checks and locks the accounts before a job can be enqueued under their authority.
async fn pause_if_lost(conn: &mut sqlx::PgConnection, s: &Schedule) -> Result<bool> {
    let principal = s.principal.strip_prefix("user:");
    let actor = principal.and_then(|id| Uuid::parse_str(id).ok());
    if principal.is_none() && s.on_behalf_of.is_none() {
        return Ok(false);
    }
    let accounts: Vec<(Uuid, String, bool)> = sqlx::query_as(
        "SELECT id, role, disabled_at IS NOT NULL FROM rookery.users
         WHERE id = $1 OR id = $2 ORDER BY id FOR SHARE",
    )
    .bind(actor)
    .bind(s.on_behalf_of)
    .fetch_all(&mut *conn)
    .await?;
    let reason = if principal.is_some()
        && !accounts
            .iter()
            .any(|(id, role, disabled)| Some(*id) == actor && role == "admin" && !disabled)
    {
        "the person it runs as is no admin with an account that is on"
    } else if accounts
        .iter()
        .any(|(id, _, disabled)| Some(*id) == s.on_behalf_of && *disabled)
    {
        "the account of the person it was made for is off"
    } else {
        return Ok(false);
    };

    sqlx::query("UPDATE rookery.schedules SET enabled = false, updated_at = now(), paused_reason = $2 WHERE id = $1")
        .bind(s.id)
        .bind(reason)
        .execute(&mut *conn)
        .await?;
    if let Some((owner, _, _)) = accounts
        .iter()
        .find(|(id, _, _)| Some(*id) == actor.or(s.on_behalf_of))
    {
        let title = format!(
            "The schedule {} was paused",
            s.title.as_deref().unwrap_or(&s.name)
        );
        let data = serde_json::json!({ "schedule": s.id.to_string(), "plugin": s.plugin });
        crate::notify::send_on(
            conn,
            crate::notify::New {
                user: *owner,
                kind: "rookery/schedulePaused",
                title: &title,
                body: Some(reason),
                data: Some(&data),
                actions: None,
                sent_by: "system:rookery",
            },
        )
        .await?;
    }
    tracing::info!(schedule = %s.id, %reason, "a schedule was paused");
    Ok(true)
}

pub struct Schedules<'a> {
    pub(crate) store: &'a Store,
}

impl Schedules<'_> {
    /// Makes or replaces the schedule of this plugin, name, space and person.
    pub async fn upsert(&self, s: &NewSchedule) -> Result<Schedule> {
        self.upsert_as(s, false).await
    }

    async fn upsert_as(&self, s: &NewSchedule, declared: bool) -> Result<Schedule> {
        let tz = s.timezone.clone().unwrap_or_else(|| "UTC".into());
        let missed = check_missed(s.missed.as_deref())?;
        let next = next_run_in(s.cron.as_deref(), s.interval_secs, &tz, Utc::now())?;
        let mut tx = self.store.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('rookery.schedules.' || $1))")
            .bind(&s.plugin)
            .execute(&mut *tx)
            .await?;
        let (count, exists): (i64, bool) = sqlx::query_as(
            "SELECT count(*), coalesce(bool_or(name = $2 AND space_id IS NOT DISTINCT FROM $3
                 AND on_behalf_of IS NOT DISTINCT FROM $4), false)
             FROM rookery.schedules WHERE plugin = $1",
        )
        .bind(&s.plugin)
        .bind(&s.name)
        .bind(s.space_id)
        .bind(s.on_behalf_of)
        .fetch_one(&mut *tx)
        .await?;
        if !exists && count >= MAX_PER_PLUGIN {
            return Err(Error::invalid(format!(
                "{} has {MAX_PER_PLUGIN} schedules, as many as a plugin may",
                s.plugin
            )));
        }
        let made = sqlx::query_as(
            "INSERT INTO rookery.schedules (id, plugin, name, title, kind, params, subject, cron, interval_secs,
                 timezone, missed, principal, on_behalf_of, space_id, enabled, declared, next_run_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
             ON CONFLICT ON CONSTRAINT schedules_key DO UPDATE SET title = EXCLUDED.title, kind = EXCLUDED.kind,
                 params = EXCLUDED.params, subject = EXCLUDED.subject, cron = EXCLUDED.cron,
                 interval_secs = EXCLUDED.interval_secs, timezone = EXCLUDED.timezone, missed = EXCLUDED.missed,
                 principal = EXCLUDED.principal, enabled = EXCLUDED.enabled, declared = EXCLUDED.declared,
                 next_run_at = EXCLUDED.next_run_at, paused_reason = NULL, updated_at = now()
             RETURNING *",
        )
        .bind(new_id())
        .bind(&s.plugin)
        .bind(&s.name)
        .bind(&s.title)
        .bind(&s.kind)
        .bind(object(&s.params))
        .bind(&s.subject)
        .bind(&s.cron)
        .bind(s.interval_secs)
        .bind(&tz)
        .bind(missed)
        .bind(&s.principal)
        .bind(s.on_behalf_of)
        .bind(s.space_id)
        .bind(s.enabled)
        .bind(declared)
        .bind(next)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(made)
    }

    /// Makes a plugin's declared schedules what its manifest says: new ones are made, changed ones
    /// changed (paused ones stay paused, and the next run stays unless the timing changed), and those it
    /// no longer declares deleted.
    pub async fn sync_declared(&self, plugin: &str, defs: &[NewSchedule]) -> Result<Vec<Schedule>> {
        let existing = self
            .list(&ScheduleQuery {
                plugin: Some(plugin.into()),
                declared: Some(true),
                ..Default::default()
            })
            .await?;
        let mut out = Vec::new();
        for d in defs {
            if d.plugin != plugin {
                return Err(Error::invalid("a declared schedule is its plugin's"));
            }
            let tz = d.timezone.clone().unwrap_or_else(|| "UTC".into());
            let missed = check_missed(d.missed.as_deref())?;
            match existing
                .iter()
                .find(|e| e.name == d.name && e.on_behalf_of.is_none())
            {
                None => out.push(self.upsert_as(d, true).await?),
                Some(e) => {
                    let timing_changed =
                        e.cron != d.cron || e.interval_secs != d.interval_secs || e.timezone != tz;
                    let next = if timing_changed || e.next_run_at.is_none() {
                        Some(next_run_in(
                            d.cron.as_deref(),
                            d.interval_secs,
                            &tz,
                            Utc::now(),
                        )?)
                    } else {
                        e.next_run_at
                    };
                    out.push(
                        sqlx::query_as(
                            "UPDATE rookery.schedules SET title = $2, kind = $3, params = $4, subject = $5, cron = $6,
                                 interval_secs = $7, timezone = $8, missed = $9, principal = $10, next_run_at = $11,
                                 updated_at = now()
                             WHERE id = $1 RETURNING *",
                        )
                        .bind(e.id)
                        .bind(&d.title)
                        .bind(&d.kind)
                        .bind(object(&d.params))
                        .bind(&d.subject)
                        .bind(&d.cron)
                        .bind(d.interval_secs)
                        .bind(&tz)
                        .bind(missed)
                        .bind(&d.principal)
                        .bind(next)
                        .fetch_one(self.store.pool())
                        .await?,
                    );
                }
            }
        }
        for e in existing {
            if !defs.iter().any(|d| d.name == e.name) {
                self.delete(e.id).await?;
            }
        }
        Ok(out)
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<Schedule>> {
        Ok(
            sqlx::query_as("SELECT * FROM rookery.schedules WHERE id = $1")
                .bind(id)
                .fetch_optional(self.store.pool())
                .await?,
        )
    }

    pub async fn list(&self, q: &ScheduleQuery) -> Result<Vec<Schedule>> {
        Ok(sqlx::query_as(
            "SELECT * FROM rookery.schedules WHERE ($1::text IS NULL OR plugin = $1)
               AND ($2::uuid IS NULL OR on_behalf_of = $2) AND ($3::boolean IS NULL OR declared = $3)
             ORDER BY plugin, name, created_at",
        )
        .bind(&q.plugin)
        .bind(q.user)
        .bind(q.declared)
        .fetch_all(self.store.pool())
        .await?)
    }

    /// Pauses or resumes. A resumed schedule runs next at its next time from now, so that the runs of the
    /// pause do not count as missed.
    pub async fn set_enabled(&self, id: Uuid, enabled: bool) -> Result<Schedule> {
        let s = self
            .get(id)
            .await?
            .ok_or_else(|| Error::not_found("no such schedule"))?;
        let next = if enabled && !s.enabled {
            Some(next_run_in(
                s.cron.as_deref(),
                s.interval_secs,
                &s.timezone,
                Utc::now(),
            )?)
        } else {
            s.next_run_at
        };
        Ok(sqlx::query_as(
            "UPDATE rookery.schedules SET enabled = $2, next_run_at = $3,
                 paused_reason = CASE WHEN $2 THEN NULL ELSE paused_reason END, updated_at = now()
             WHERE id = $1 RETURNING *",
        )
        .bind(id)
        .bind(enabled)
        .bind(next)
        .fetch_one(self.store.pool())
        .await?)
    }

    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        Ok(sqlx::query("DELETE FROM rookery.schedules WHERE id = $1")
            .bind(id)
            .execute(self.store.pool())
            .await?
            .rows_affected()
            > 0)
    }

    /// Fires an enabled schedule now with `extra` merged into its params. Its timing stays.
    pub async fn fire(&self, id: Uuid, extra: Option<&Value>) -> Result<Job> {
        let mut tx = self.store.pool().begin().await?;
        sqlx::query("SELECT rookery.fence($1)")
            .bind(self.store.fence())
            .execute(&mut *tx)
            .await?;
        let s: Schedule =
            sqlx::query_as("SELECT * FROM rookery.schedules WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| Error::not_found("no such schedule"))?;
        if !s.enabled {
            return Err(Error::forbidden("the schedule is paused"));
        }
        if pause_if_lost(&mut tx, &s).await? {
            tx.commit().await?;
            return Err(Error::forbidden(
                "the schedule was paused because its principal lost access",
            ));
        }
        let mut params = object(&s.params);
        if let (Some(p), Some(Value::Object(e))) = (params.as_object_mut(), extra) {
            for (k, v) in e {
                p.insert(k.clone(), v.clone());
            }
        }
        let job = enqueue(&mut tx, &s, &params, self.store.fence()).await?;
        sqlx::query(
            "UPDATE rookery.schedules SET last_run_at = now(), last_job_id = $2, updated_at = now() WHERE id = $1",
        )
        .bind(s.id)
        .bind(job.id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(job)
    }

    /// Enqueues each due schedule's jobs, or pauses it and notifies its owner if it lost its authority.
    pub async fn enqueue_due(&self, now: DateTime<Utc>) -> Result<Vec<Job>> {
        let mut tx = self.store.pool().begin().await?;
        sqlx::query("SELECT rookery.fence($1)")
            .bind(self.store.fence())
            .execute(&mut *tx)
            .await?;
        let due: Vec<Schedule> = sqlx::query_as(
            "SELECT * FROM rookery.schedules WHERE enabled AND next_run_at <= $1 ORDER BY next_run_at LIMIT 1000
             FOR UPDATE SKIP LOCKED",
        )
        .bind(now)
        .fetch_all(&mut *tx)
        .await?;
        let mut jobs = Vec::new();
        for s in due {
            if pause_if_lost(&mut tx, &s).await? {
                continue;
            }
            let due_at = s.next_run_at.unwrap_or(now);
            let late = now - due_at > MISSED_AFTER;
            let mut runs: Vec<DateTime<Utc>> = Vec::new();
            match (late, s.missed.as_str()) {
                (false, _) | (true, "once") => runs.push(due_at),
                (true, "skip") => {}
                (true, _) => {
                    let mut t = due_at;
                    while t <= now && runs.len() < MAX_CATCH_UP {
                        runs.push(t);
                        t = match next_run_in(s.cron.as_deref(), s.interval_secs, &s.timezone, t) {
                            Ok(n) => n,
                            Err(_) => break,
                        };
                    }
                }
            }
            // A run due while the last job is still open starts no second one; runs caught up on each count.
            let open: bool = match s.last_job_id {
                Some(job) if runs.len() == 1 => sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM rookery.jobs WHERE id = $1 AND state IN ('queued', 'running'))",
                )
                .bind(job)
                .fetch_one(&mut *tx)
                .await?,
                _ => false,
            };
            let mut last_job = s.last_job_id;
            if !open {
                for _ in &runs {
                    let job = enqueue(&mut tx, &s, &object(&s.params), self.store.fence()).await?;
                    last_job = Some(job.id);
                    jobs.push(job);
                }
            }
            let next = match next_run_in(s.cron.as_deref(), s.interval_secs, &s.timezone, now) {
                Ok(n) => Some(n),
                Err(e) => {
                    tracing::warn!(schedule = %s.id, error = %e, "a schedule cannot run again: it is paused");
                    None
                }
            };
            sqlx::query(
                "UPDATE rookery.schedules SET last_run_at = CASE WHEN $4 THEN $2 ELSE last_run_at END, next_run_at = $3,
                     enabled = enabled AND $3 IS NOT NULL, last_job_id = $5, updated_at = now() WHERE id = $1",
            )
            .bind(s.id)
            .bind(now)
            .bind(next)
            .bind(!runs.is_empty() && !open)
            .bind(last_job)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(jobs)
    }
}
