//! The leader lock: a Postgres advisory lock on a connection of its own. Only one server per database
//! holds it; the others wait on it as standbys.
//!
//! Postgres hands the lock to a standby as soon as the leader's lock connection ends, and the old leader
//! learns of it only at its next check. So the server that takes the lock also increments the leader's
//! epoch ([`rookery_store::leader`]), and its writes to jobs and leases name it: the writes of a leader
//! that was deposed without knowing yet fail. A leader whose check fails, or does not answer in time,
//! stops leading at once.

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

/// How often a leader checks that it still holds the lock.
const CHECK_EVERY: Duration = Duration::from_secs(1);
/// How long a check may take. One that takes longer counts as a lost lock: a leader that cannot
/// confirm it leads stops.
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);

pub struct Lock {
    conn: PgConnection,
    epoch: Option<i64>,
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
        Ok(Lock { conn, epoch: None })
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
        Ok(held.then_some(Lock { conn, epoch: None }))
    }

    /// Starts this server's epoch, once the database is migrated. Its writes name it from now on.
    pub async fn take_epoch(&mut self) -> Result<i64, String> {
        let epoch =
            tokio::time::timeout(CHECK_TIMEOUT, rookery_store::leader::take(&mut self.conn))
                .await
                .map_err(|_| "the database did not answer in time".to_owned())?
                .map_err(|e| format!("starting the leader's epoch: {e}"))?;
        self.epoch = Some(epoch);
        Ok(epoch)
    }

    pub fn epoch(&self) -> Option<i64> {
        self.epoch
    }

    /// Returns when this server can no longer say it holds the lock, and says why: the lock's
    /// connection is gone or does not answer in time, or another server's epoch began.
    pub async fn lost(&mut self) -> String {
        let mut every = tokio::time::interval(CHECK_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            let check = tokio::time::timeout(CHECK_TIMEOUT, async {
                match self.epoch {
                    Some(_) => rookery_store::leader::epoch(&mut self.conn)
                        .await
                        .map_err(|e| e.to_string()),
                    None => sqlx::query("SELECT 1")
                        .execute(&mut self.conn)
                        .await
                        .map(|_| None)
                        .map_err(|e| e.to_string()),
                }
            })
            .await;
            match check {
                Err(_) => {
                    return format!(
                        "the connection that holds the leader lock did not answer within {}s",
                        CHECK_TIMEOUT.as_secs()
                    );
                }
                Ok(Err(e)) => {
                    return format!("the connection that holds the leader lock is gone: {e}");
                }
                Ok(Ok(now)) if self.epoch.is_some() && now != self.epoch => {
                    return format!(
                        "another server leads at epoch {}",
                        now.map_or("?".into(), |e| e.to_string())
                    );
                }
                Ok(Ok(_)) => {}
            }
        }
    }
}
