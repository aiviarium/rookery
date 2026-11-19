//! The leader's epoch. A server that takes the leader lock increments it, and its store names it in
//! every write to jobs and leases ([`crate::Store::fenced`]). Once another server took the lock, those
//! writes fail with `UNAVAILABLE`, so a leader that lost its lock without knowing yet changes nothing.

use sqlx::PgConnection;

use crate::error::Result;

/// The Postgres error code `rookery.fence` raises for a write of a server that does not lead any more.
pub const DEPOSED: &str = "RK001";

/// How the message of that error starts.
pub(crate) const DEPOSED_MESSAGE: &str = "this server does not lead any more";

/// Increments the epoch and returns it. Called by the server that just took the leader lock.
pub async fn take(conn: &mut PgConnection) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "INSERT INTO rookery.leader AS l (id, epoch) VALUES (true, 1)
         ON CONFLICT (id) DO UPDATE SET epoch = l.epoch + 1 RETURNING epoch",
    )
    .fetch_one(conn)
    .await?)
}

/// The epoch now, if a server ever led.
pub async fn epoch(conn: &mut PgConnection) -> Result<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT epoch FROM rookery.leader WHERE id")
            .fetch_optional(conn)
            .await?,
    )
}
