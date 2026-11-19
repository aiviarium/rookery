//! The locks of subjects as leases in `rookery.leases`, so that two servers on one database, or a
//! leader and the one that takes over from it, do not change a host at once. A lease is renewed while it
//! is held and lapses on its own when the server that held it is gone.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use rookery_core::runner::{Holder, Lapse, Locks};
use sqlx::PgPool;

use crate::db::Db;

/// How long a lease lasts without being renewed.
const TTL: Duration = Duration::from_secs(60);

/// The plugin whose leases these are, in the names of `rookery.leases`.
const PREFIX: &str = "rookery/";

/// How often a held lease is renewed, and how often again after a renewal failed.
const RENEW: Duration = Duration::from_secs(20);
const RETRY: Duration = Duration::from_secs(2);

pub struct LeaseLocks {
    db: Db,
    /// A connection of their own for renewals, so that they never wait behind slow queries.
    renewals: Db,
    /// The leader's epoch the leases are taken in; none outside a leading server.
    fence: Option<i64>,
}

impl LeaseLocks {
    pub fn new(db: Db, renewals: Db, fence: Option<i64>) -> Arc<LeaseLocks> {
        Arc::new(LeaseLocks {
            db,
            renewals,
            fence,
        })
    }
}

fn store(pool: PgPool, fence: Option<i64>) -> rookery_store::Store {
    let store = rookery_store::Store::from_pool(pool);
    match fence {
        Some(epoch) => store.fenced(epoch),
        None => store,
    }
}

/// A held lease: renewed until it is dropped, then released.
struct Held {
    stop: mpsc::Sender<()>,
    db: Db,
    fence: Option<i64>,
    name: String,
    holder: String,
    lease: i64,
}

impl Drop for Held {
    /// Released before the drop returns, so the next one to ask gets it.
    fn drop(&mut self) {
        let _ = self.stop.send(());
        let (name, holder, fence, epoch) = (
            self.name.clone(),
            self.holder.clone(),
            self.lease,
            self.fence,
        );
        self.db.run(move |pool: PgPool| {
            Box::pin(async move {
                let _ = store(pool, epoch)
                    .leases()
                    .release(&name, &holder, fence)
                    .await;
            })
        });
    }
}

/// Holders are `DOING · ID`: what they do, which a refusal shows, and what makes each one its own.
const SEPARATOR: &str = " · ";

impl Locks for LeaseLocks {
    fn take(&self, name: &str, doing: &str, lapse: Arc<Lapse>) -> Result<Box<dyn Send>, Holder> {
        let name = format!("{PREFIX}{name}");
        let holder = format!("{doing}{SEPARATOR}{}", uuid::Uuid::now_v7());
        let epoch = self.fence;
        let taken = {
            let (name, holder) = (name.clone(), holder.clone());
            self.db.run(move |pool: PgPool| {
                Box::pin(async move {
                    let store = store(pool, epoch);
                    match store.leases().acquire(&name, &holder, TTL).await {
                        Ok(lease) => Ok(Ok(lease.fence)),
                        Err(rookery_store::Error::Conflict(_)) => {
                            let held = store.leases().inspect(&name).await.ok().flatten();
                            Ok(Err(held.and_then(|l| l.holder).unwrap_or_default()))
                        }
                        Err(e) => Err(e.to_string()),
                    }
                })
            })
        };
        let fence = match taken {
            Ok(Ok(fence)) => fence,
            Ok(Err(holder)) => {
                let doing = holder
                    .split(SEPARATOR)
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                return Err(Holder::Held(doing));
            }
            Err(e) => return Err(Holder::Failed(format!("taking the lease {name}: {e}"))),
        };
        let (stop, stopped) = mpsc::channel();
        let held = Held {
            stop,
            db: self.db.clone(),
            fence: epoch,
            name: name.clone(),
            holder: holder.clone(),
            lease: fence,
        };
        let db = self.renewals.clone();
        std::thread::spawn(move || {
            // Renewed in time, the lease lasts TTL from then. A renewal that fails, or does not
            // answer before the lease would end, is tried again; after TTL/2 of failing the work
            // that holds it stops, while the lease still holds.
            let mut until = std::time::Instant::now() + TTL;
            let mut failing: Option<std::time::Instant> = None;
            let mut wait = RENEW;
            loop {
                match stopped.recv_timeout(wait) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let (lease, holder) = (name.clone(), holder.clone());
                        let asked = std::time::Instant::now();
                        let left = until.saturating_duration_since(asked);
                        let renewed = db.run(move |pool: PgPool| {
                            Box::pin(async move {
                                match tokio::time::timeout(
                                    left,
                                    store(pool, epoch)
                                        .leases()
                                        .renew(&lease, &holder, fence, TTL),
                                )
                                .await
                                {
                                    Ok(done) => done.map(|_| ()).map_err(Some),
                                    Err(_) => Err(None),
                                }
                            })
                        });
                        match renewed {
                            Ok(()) => {
                                until = asked + TTL;
                                failing = None;
                                wait = RENEW;
                            }
                            // Another holds it now: it is gone at once.
                            Err(Some(rookery_store::Error::Conflict(_))) => {
                                tracing::warn!(lease = %name, "a lease was lost while it was held");
                                lapse.lost(&format!("losing the lease {name}"));
                                return;
                            }
                            Err(e) => {
                                let why = match e {
                                    Some(e) => e.to_string(),
                                    None => "the database did not answer in time".to_owned(),
                                };
                                let since = *failing.get_or_insert(asked);
                                if since.elapsed() >= TTL / 2
                                    || std::time::Instant::now() + RETRY >= until
                                {
                                    tracing::warn!(lease = %name, error = %why, "a lease could not be renewed: its work stops");
                                    lapse.lost(&format!(
                                        "the lease {name}, which could not be renewed: {why}"
                                    ));
                                    return;
                                }
                                tracing::warn!(lease = %name, error = %why, "renewing a lease failed: trying again");
                                wait = RETRY;
                            }
                        }
                    }
                    // The lock is let go.
                    _ => return,
                }
            }
        });
        Ok(Box::new(held))
    }
}
