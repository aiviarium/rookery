//! Leases with fencing. A lease is held until it expires or is released. Its fence grows each time
//! it changes hands, so a holder whose lease lapsed can be told apart from the one that has it now.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Lease {
    pub name: String,
    pub holder: Option<String>,
    pub fence: i64,
    pub expires_at: Option<DateTime<Utc>>,
}

const COLS: &str = "name, holder, fence, expires_at";

pub struct Leases<'a> {
    pub(crate) pool: &'a PgPool,
}

fn ttl(ttl: std::time::Duration) -> Result<f64> {
    if ttl.is_zero() || ttl > std::time::Duration::from_secs(24 * 3600) {
        return Err(Error::Invalid(
            "a lease lasts from a millisecond to a day".into(),
        ));
    }
    Ok(ttl.as_secs_f64())
}

impl Leases<'_> {
    /// Takes a lease that is free, has expired, or is the holder's already (which keeps its fence).
    pub async fn acquire(
        &self,
        name: &str,
        holder: &str,
        duration: std::time::Duration,
    ) -> Result<Lease> {
        let secs = ttl(duration)?;
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "INSERT INTO rookery.leases AS l (name, holder, fence, expires_at) VALUES ($1, $2, 1, now() + make_interval(secs => $3))
             ON CONFLICT (name) DO UPDATE SET
                 fence = CASE WHEN l.holder = $2 AND l.expires_at > now() THEN l.fence ELSE l.fence + 1 END,
                 holder = $2, expires_at = now() + make_interval(secs => $3)
             WHERE l.holder IS NULL OR l.expires_at <= now() OR l.holder = $2
             RETURNING {COLS}"
        )))
        .bind(name)
        .bind(holder)
        .bind(secs)
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| Error::Conflict(format!("the lease {name} is held by another")))
    }

    /// Extends a lease the holder has, at that fence.
    pub async fn renew(
        &self,
        name: &str,
        holder: &str,
        fence: i64,
        duration: std::time::Duration,
    ) -> Result<Lease> {
        let secs = ttl(duration)?;
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE rookery.leases SET expires_at = now() + make_interval(secs => $4)
             WHERE name = $1 AND holder = $2 AND fence = $3 AND expires_at > now() RETURNING {COLS}"
        )))
        .bind(name)
        .bind(holder)
        .bind(fence)
        .bind(secs)
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| {
            Error::Conflict(format!(
                "the lease {name} at fence {fence} is not the holder's any more"
            ))
        })
    }

    pub async fn release(&self, name: &str, holder: &str, fence: i64) -> Result<bool> {
        let done = sqlx::query("UPDATE rookery.leases SET holder = NULL, expires_at = NULL WHERE name = $1 AND holder = $2 AND fence = $3")
            .bind(name)
            .bind(holder)
            .bind(fence)
            .execute(self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    /// A lease as it is now. An expired one shows no holder.
    pub async fn inspect(&self, name: &str) -> Result<Option<Lease>> {
        Ok(sqlx::query_as(
            "SELECT name, CASE WHEN expires_at > now() THEN holder END AS holder, fence,
                    CASE WHEN expires_at > now() THEN expires_at END AS expires_at
             FROM rookery.leases WHERE name = $1",
        )
        .bind(name)
        .fetch_optional(self.pool)
        .await?)
    }
}
