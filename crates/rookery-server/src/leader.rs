//! The leader lock: a Postgres advisory lock on a connection of its own. Only one server per database
//! holds it; the others wait on it as standbys. A leader that loses the connection stops leading.

use std::time::Duration;

use sqlx::{Connection, PgConnection};

/// Whether this server leads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Waits for the lock and answers 503.
    Standby,
    Leader,
}

/// The advisory lock's key, the same for every server of a database.
pub const LOCK_KEY: i64 = 0x726f_6f6b_6c65_6164;

/// How often a leader checks that its lock's connection is there.
const CHECK_EVERY: Duration = Duration::from_secs(5);

pub struct Lock {
    conn: PgConnection,
}

impl Lock {
    /// Waits until the lock is this server's. Postgres hands it over when the leader's connection ends.
    pub async fn acquire(url: &str) -> Result<Lock, String> {
        let mut conn = PgConnection::connect(url)
            .await
            .map_err(|e| format!("connecting for the leader lock: {e}"))?;
        sqlx::query("SET application_name = 'rookery leader lock'")
            .execute(&mut conn)
            .await
            .map_err(|e| e.to_string())?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(LOCK_KEY)
            .execute(&mut conn)
            .await
            .map_err(|e| format!("waiting for the leader lock: {e}"))?;
        Ok(Lock { conn })
    }

    /// The lock if no server holds it, without waiting.
    pub async fn try_acquire(url: &str) -> Result<Option<Lock>, String> {
        let mut conn = PgConnection::connect(url)
            .await
            .map_err(|e| format!("connecting for the leader lock: {e}"))?;
        let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(LOCK_KEY)
            .fetch_one(&mut conn)
            .await
            .map_err(|e| e.to_string())?;
        Ok(held.then_some(Lock { conn }))
    }

    /// Returns when the lock's connection is gone, and says why.
    pub async fn lost(&mut self) -> String {
        let mut every = tokio::time::interval(CHECK_EVERY);
        loop {
            every.tick().await;
            if let Err(e) = sqlx::query("SELECT 1").execute(&mut self.conn).await {
                return format!(
                    "the connection that holds the leader lock is gone, so this server stops leading: {e}"
                );
            }
        }
    }
}
