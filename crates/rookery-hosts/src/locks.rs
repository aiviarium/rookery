//! The locks of subjects as leases in `rookery.leases`, so that two servers on one database, or a
//! leader and the one that takes over from it, do not change a host at once. A lease is renewed while it
//! is held and lapses on its own when the server that held it is gone.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use rookery_core::runner::{Holder, Locks};
use sqlx::PgPool;

use crate::db::Db;

/// How long a lease lasts without being renewed.
const TTL: Duration = Duration::from_secs(60);

/// The plugin whose leases these are, in the names of `rookery.leases`.
const PREFIX: &str = "rookery/";

pub struct LeaseLocks {
    db: Db,
    /// The leader's epoch the leases are taken in; none outside a leading server.
    fence: Option<i64>,
}

impl LeaseLocks {
    pub fn new(db: Db, fence: Option<i64>) -> Arc<LeaseLocks> {
        Arc::new(LeaseLocks { db, fence })
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
    fn take(&self, name: &str, doing: &str) -> Result<Box<dyn Send>, Holder> {
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
        let db = self.db.clone();
        std::thread::spawn(move || {
            loop {
                match stopped.recv_timeout(TTL / 3) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let (lease, holder) = (name.clone(), holder.clone());
                        let renewed = db.run(move |pool: PgPool| {
                            Box::pin(async move {
                                store(pool, epoch)
                                    .leases()
                                    .renew(&lease, &holder, fence, TTL)
                                    .await
                                    .is_ok()
                            })
                        });
                        if !renewed {
                            tracing::warn!(lease = %name, "a lease lapsed while it was held");
                            return;
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
