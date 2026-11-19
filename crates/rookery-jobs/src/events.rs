//! `events.*`: at most once. An event reaches the connections subscribed to it at that moment, as an
//! `events.event` notification, and is kept nowhere.
//!
//! A connection that does not keep up loses events instead of closing: a burst, such as every host going
//! offline at a takeover, fills its queue, and what does not fit is dropped. The subscription counts the
//! drops and, once the connection has room again, says how many with an `events.dropped` notification,
//! so that the client reads again what it shows or subscribes again.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rookery_kernel::conn::ConnHandle;
use rookery_kernel::{Call, Empty, Kernel, Level, Methods, RpcError};
use rookery_proto::jobs::{
    Delivered, EventNotice, EventPublish, EventSubscribe, EventSubscribed, EventUnsubscribe,
    EventsDropped,
};
use serde_json::Value;

use crate::scoped;

struct Subscription {
    conn: ConnHandle,
    names: Vec<String>,
    drops: Arc<Drops>,
}

/// The events a subscription dropped and has not said yet.
#[derive(Default)]
struct Drops {
    pending: AtomicU64,
    /// A task waits to say them.
    telling: AtomicBool,
}

/// How often a connection that dropped events is looked at for room to say so.
const TELL_EVERY: Duration = Duration::from_millis(100);

#[derive(Default)]
pub struct Subscriptions(Mutex<HashMap<String, Subscription>>);

fn matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

/// Sends an event to the connections subscribed to it, for code inside the server as for
/// `events.publish`. With `user`, to that person's connections alone. Returns how many it reached.
pub fn publish(kernel: &Kernel, name: &str, payload: &Value, user: Option<&str>) -> usize {
    send(kernel, name, payload, user).delivered
}

/// [`publish`], with how many subscribed connections were too slow for it.
pub fn send(kernel: &Kernel, name: &str, payload: &Value, user: Option<&str>) -> Delivered {
    let subscriptions = kernel.facility::<Subscriptions>();
    let all = subscriptions.0.lock().unwrap();
    let mut done = Delivered {
        delivered: 0,
        dropped: 0,
    };
    for (id, s) in all.iter() {
        let reaches = user.is_none_or(|u| s.conn.caller().user().is_some_and(|me| me.id == u));
        if reaches && s.names.iter().any(|p| matches(p, name)) {
            let notice = EventNotice {
                subscription: id.clone(),
                name: name.to_owned(),
                payload: payload.clone(),
            };
            if s.conn.offer("events.event", notice) {
                done.delivered += 1;
            } else {
                done.dropped += 1;
                s.drops.pending.fetch_add(1, Ordering::Relaxed);
                if !s.drops.telling.swap(true, Ordering::AcqRel) {
                    tokio::spawn(tell_drops(
                        kernel.clone(),
                        id.clone(),
                        s.conn.clone(),
                        s.drops.clone(),
                    ));
                }
            }
        }
    }
    done
}

/// Waits until the connection has room, and says how many events the subscription dropped.
async fn tell_drops(kernel: Kernel, subscription: String, conn: ConnHandle, drops: Arc<Drops>) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(TELL_EVERY) => {}
            _ = kernel.shutdown.cancelled() => return,
        }
        let subscribed = kernel
            .facility::<Subscriptions>()
            .0
            .lock()
            .unwrap()
            .contains_key(&subscription);
        if conn.is_closed() || !subscribed {
            return;
        }
        let dropped = drops.pending.load(Ordering::Acquire);
        if dropped == 0 {
            drops.telling.store(false, Ordering::Release);
            // A drop that came meanwhile found the task still telling: it is this task's.
            if drops.pending.load(Ordering::Acquire) == 0
                || drops.telling.swap(true, Ordering::AcqRel)
            {
                return;
            }
            continue;
        }
        let told = conn.offer(
            "events.dropped",
            EventsDropped {
                subscription: subscription.clone(),
                dropped,
            },
        );
        if told {
            drops.pending.fetch_sub(dropped, Ordering::AcqRel);
        }
    }
}

pub fn register(methods: &mut Methods) {
    methods.on_connection_closed(|kernel, conn| {
        kernel
            .facility::<Subscriptions>()
            .0
            .lock()
            .unwrap()
            .retain(|_, s| s.conn.id() != conn.id());
    });
    methods
        .add("events.publish", |call: Call, p: EventPublish| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let name = scoped(&plugin, &p.name)?;
            // Connections name their person by the id's one spelling.
            let user = p
                .user
                .as_deref()
                .map(|id| {
                    uuid::Uuid::parse_str(id)
                        .map(|id| id.to_string())
                        .map_err(|_| RpcError::invalid(format!("{id:?} is not the id of a user")))
                })
                .transpose()?;
            Ok(send(&call.kernel, &name, &p.payload, user.as_deref()))
        })
        .summary("Sends an event of the plugin to the connections subscribed now, at most once: a connection too slow for it loses it, and later hears events.dropped.")
        .level(Level::Plugin);
    methods
        .add("events.subscribe", |call: Call, p: EventSubscribe| async move {
            let conn = call.conn()?.clone();
            if p.names.is_empty() || p.names.len() > 100 || p.names.iter().any(|n| n.is_empty() || n.len() > 300) {
                return Err(RpcError::invalid("subscribe to 1 to 100 names, such as fleet/host.changed or fleet/*"));
            }
            let id = uuid::Uuid::now_v7().to_string();
            call.kernel.facility::<Subscriptions>().0.lock().unwrap().insert(id.clone(), Subscription { conn, names: p.names, drops: Arc::default() });
            Ok(EventSubscribed { subscription: id })
        })
        .summary("Events by name, or by a prefix ending in *, as events.event notifications on this connection.")
        .level(Level::Own);
    methods
        .add(
            "events.unsubscribe",
            |call: Call, p: EventUnsubscribe| async move {
                let conn = call.conn()?.id();
                let subscriptions = call.kernel.facility::<Subscriptions>();
                let mut all = subscriptions.0.lock().unwrap();
                match all.get(&p.subscription) {
                    Some(s) if s.conn.id() == conn => {
                        all.remove(&p.subscription);
                        Ok(Empty {})
                    }
                    _ => Err(RpcError::not_found(
                        "no such subscription on this connection",
                    )),
                }
            },
        )
        .summary("Ends a subscription to events.")
        .level(Level::Own);
}
