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
}

impl LeaseLocks {
    pub fn new(db: Db) -> Arc<LeaseLocks> {
        Arc::new(LeaseLocks { db })
    }
}

/// A held lease: renewed until it is dropped, then released.
struct Held {
    stop: mpsc::Sender<()>,
    db: Db,
    name: String,
    holder: String,
    fence: i64,
}

impl Drop for Held {
    /// Released before the drop returns, so the next one to ask gets it.
    fn drop(&mut self) {
        let _ = self.stop.send(());
        let (name, holder, fence) = (self.name.clone(), self.holder.clone(), self.fence);
        self.db.run(move |pool: PgPool| {
            Box::pin(async move {
                let _ = rookery_store::Store::from_pool(pool)
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
        let taken = {
            let (name, holder) = (name.clone(), holder.clone());
            self.db.run(move |pool: PgPool| {
                Box::pin(async move {
                    let store = rookery_store::Store::from_pool(pool);
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
            name: name.clone(),
            holder: holder.clone(),
            fence,
        };
        let db = self.db.clone();
        std::thread::spawn(move || {
            loop {
                match stopped.recv_timeout(TTL / 3) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let (lease, holder) = (name.clone(), holder.clone());
                        let renewed = db.run(move |pool: PgPool| {
                            Box::pin(async move {
                                rookery_store::Store::from_pool(pool)
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
