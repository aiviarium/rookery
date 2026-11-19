//! Live queries and the change feed, over a connection on `/ws`.
//!
//! Each subscription is a task with a subscription of its own to the store's feed, so it gets every
//! change after its cursor once and in order: from memory after commit when it keeps up, from the
//! change log when it lags or resumes. `data.live` sends the changes of one collection in the
//! subscription's spaces, each with the row as the subscriber may read it now (`null` when it may not,
//! or the row is gone); the client applies them to its result with its own copy of the filter. A
//! plugin's `data.subscribe` sends every change of its collections. A subscriber whose cursor is older
//! than the log gets `reset` and reads again. Subscriptions end with their connection.

use std::collections::HashMap;
use std::sync::Mutex;

use rookery_kernel::{Call, Caller, Empty, Level, Methods, RpcError};
use rookery_proto::data::{
    Change, ChangeOp, DataChanged, FeedSubscribe, FeedSubscription, LiveQuery, LiveStarted,
    SubscriptionRef,
};
use rookery_store::collections::Scope;
use rookery_store::feed::{Change as StoreChange, ChangeOp as StoreOp, FeedEvent};
use rookery_store::{Caller as StoreCaller, Level as StoreLevel};
use tokio::task::AbortHandle;
use uuid::Uuid;

use crate::data::query;
use crate::{rpc, target, time};

struct Sub {
    conn: u64,
    task: AbortHandle,
}

#[derive(Default)]
struct Live {
    subs: Mutex<HashMap<String, Sub>>,
}

impl Live {
    fn add(&self, id: String, conn: u64, task: AbortHandle) {
        self.subs.lock().unwrap().insert(id, Sub { conn, task });
    }

    fn remove(&self, id: &str, conn: u64) -> bool {
        let mut subs = self.subs.lock().unwrap();
        if subs.get(id).is_some_and(|s| s.conn == conn)
            && let Some(s) = subs.remove(id)
        {
            s.task.abort();
            return true;
        }
        false
    }

    fn remove_all(&self, conn: u64) {
        self.subs.lock().unwrap().retain(|_, s| {
            let keep = s.conn != conn;
            if !keep {
                s.task.abort();
            }
            keep
        });
    }

    fn count(&self) -> usize {
        self.subs.lock().unwrap().len()
    }
}

/// How many live queries and feed subscriptions run, for tests and diagnostics.
pub fn running(kernel: &rookery_kernel::Kernel) -> usize {
    kernel.facility::<Live>().count()
}

pub fn register(methods: &mut Methods) {
    methods
        .add("data.live", |call: Call, p: LiveQuery| async move { live(&call, p).await })
        .summary("A query whose result stays current: its first page, then data.changed for each change. With after, resumes from a seq.")
        .level(Level::Own);
    methods
        .add("data.unlive", |call: Call, p: SubscriptionRef| async move {
            let conn = call.conn()?.id();
            call.kernel.facility::<Live>().remove(&p.subscription, conn);
            Ok(Empty {})
        })
        .summary("Ends a live query.")
        .level(Level::Own);
    methods
        .add(
            "data.subscribe",
            |call: Call, p: FeedSubscribe| async move { subscribe(&call, p).await },
        )
        .summary(
            "Every change of the plugin's collections as data.changed, from now or after a seq.",
        )
        .level(Level::Plugin);
    methods
        .add(
            "data.unsubscribe",
            |call: Call, p: SubscriptionRef| async move {
                let conn = call.conn()?.id();
                call.kernel.facility::<Live>().remove(&p.subscription, conn);
                Ok(Empty {})
            },
        )
        .summary("Ends a subscription to the change feed.")
        .level(Level::Plugin);
    methods.on_connection_closed(|kernel, conn| {
        kernel.facility::<Live>().remove_all(conn.id());
    });
}

/// A change as the wire has it, with the row the subscriber may read.
pub fn wire_change(c: &StoreChange, row: Option<serde_json::Value>) -> Change {
    Change {
        seq: c.seq,
        collection: c.collection.clone(),
        id: c.id.to_string(),
        op: match c.op {
            StoreOp::Insert => ChangeOp::Insert,
            StoreOp::Update => ChangeOp::Update,
            StoreOp::Delete => ChangeOp::Delete,
            StoreOp::Restore => ChangeOp::Restore,
        },
        rev: c.rev,
        space_id: c.space_id.map(|s| s.to_string()),
        at: time(c.at),
        row,
    }
}

async fn live(call: &Call, p: LiveQuery) -> Result<LiveStarted, RpcError> {
    let conn = call.conn()?.clone();
    let (plugin, caller) = target(call, &p.target).await?;
    let collection = p.target.collection.clone();
    let store = call.store().clone();
    let coll = store.collections().get(&plugin, &collection).map_err(rpc)?;
    if coll.is_log() {
        return Err(RpcError::invalid(
            "a log collection has no change feed: follow it with data.tail",
        ));
    }
    let q = query(&p.query)?;
    // The spaces the subscription covers. Following needs view on each: a person outside a space does
    // not follow it, even where single rows of it are shared with them.
    let spaces: Vec<Uuid> = match coll.def.scope {
        Scope::Space => match &q.spaces {
            Some(s) => s.clone(),
            None => vec![caller.space.ok_or_else(|| {
                RpcError::invalid("this collection is a space's: name the space")
            })?],
        },
        _ => Vec::new(),
    };
    for s in &spaces {
        let level = store
            .access()
            .level_of(&caller, &rookery_store::uri::space_uri(*s))
            .await
            .map_err(rpc)?;
        if level.is_none_or(|l| l < StoreLevel::View) {
            return Err(RpcError::forbidden(format!(
                "this needs view on space://{s}"
            )));
        }
    }
    if coll.def.scope == Scope::User && caller.user.is_none() {
        return Err(RpcError::invalid(
            "this collection is a person's, and the call is for none",
        ));
    }
    if let Some(w) = &q.filter {
        store.data(&plugin).filter(&collection, w).map_err(rpc)?;
    }
    let (mut sub, rows, next, included) = match p.after {
        Some(after) => (store.feed().subscribe_from(after), None, None, None),
        None => {
            // Subscribe before reading, so that nothing committed in between is missed.
            let sub = store.feed().subscribe().await.map_err(rpc)?;
            let page = store
                .data(&plugin)
                .find(&caller, &collection, &q)
                .await
                .map_err(rpc)?;
            (sub, Some(page.rows), page.next, page.included)
        }
    };
    let select = q.select.clone();
    let seq = p.after.unwrap_or_else(|| sub.last_seq());
    let id = Uuid::now_v7().to_string();
    let sid = id.clone();
    let user = caller.user;
    let conn_id = conn.id();
    let task = tokio::spawn(async move {
        loop {
            let ev = match sub.next().await {
                Ok(Some(ev)) => ev,
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!(error = %e, "a live query's feed failed; the client reads again");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    conn.notify(
                        "data.changed",
                        DataChanged {
                            subscription: sid.clone(),
                            changes: Vec::new(),
                            reset: Some(store.feed().head()),
                        },
                    );
                    continue;
                }
            };
            let c = match ev {
                FeedEvent::Reset { head } => {
                    conn.notify(
                        "data.changed",
                        DataChanged {
                            subscription: sid.clone(),
                            changes: Vec::new(),
                            reset: Some(head),
                        },
                    );
                    continue;
                }
                FeedEvent::Change(c) => c,
            };
            if c.plugin != plugin || c.collection != collection {
                continue;
            }
            let in_scope = match coll.def.scope {
                Scope::Space => c.space_id.is_some_and(|s| spaces.contains(&s)),
                Scope::User => c.user_id == user,
                Scope::Global => true,
            };
            if !in_scope {
                continue;
            }
            // The row as this subscriber may read it now: the store checks.
            let reader = StoreCaller {
                space: c.space_id.or(caller.space),
                ..caller.clone()
            };
            let row = match store.data(&plugin).get(&reader, &collection, c.id).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!(error = %e, "a live query's read failed");
                    None
                }
            };
            let row = row.map(|r| coll.project(r, select.as_deref()));
            conn.notify(
                "data.changed",
                DataChanged {
                    subscription: sid.clone(),
                    changes: vec![wire_change(&c, row)],
                    reset: None,
                },
            );
        }
    });
    call.kernel
        .facility::<Live>()
        .add(id.clone(), conn_id, task.abort_handle());
    Ok(LiveStarted {
        subscription: id,
        rows,
        next,
        included,
        seq,
    })
}

async fn subscribe(call: &Call, p: FeedSubscribe) -> Result<FeedSubscription, RpcError> {
    let plugin = match &call.caller {
        Caller::Plugin(plugin) => plugin.clone(),
        _ => {
            return Err(RpcError::forbidden(
                "the change feed of a plugin is the plugin's",
            ));
        }
    };
    let conn = call.conn()?.clone();
    let mut sub = match p.after {
        Some(seq) => call.store().feed().subscribe_from(seq),
        None => call.store().feed().subscribe().await.map_err(rpc)?,
    };
    let seq = p.after.unwrap_or_else(|| sub.last_seq());
    let id = Uuid::now_v7().to_string();
    let sid = id.clone();
    let collections = p.collections;
    let task = tokio::spawn(async move {
        loop {
            let changed = match sub.next().await {
                Ok(Some(FeedEvent::Reset { head })) => DataChanged {
                    subscription: sid.clone(),
                    changes: Vec::new(),
                    reset: Some(head),
                },
                Ok(Some(FeedEvent::Change(c))) => {
                    if c.plugin != plugin
                        || collections
                            .as_ref()
                            .is_some_and(|cs| !cs.contains(&c.collection))
                    {
                        continue;
                    }
                    DataChanged {
                        subscription: sid.clone(),
                        changes: vec![wire_change(&c, c.row.clone())],
                        reset: None,
                    }
                }
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!(error = %e, "a plugin's change feed failed; trying again");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
            };
            conn.notify("data.changed", changed);
        }
    });
    call.kernel
        .facility::<Live>()
        .add(id.clone(), call.conn()?.id(), task.abort_handle());
    Ok(FeedSubscription {
        subscription: id,
        seq,
    })
}
