//! `events.*`: at most once. An event reaches the connections subscribed to it at that moment, as an
//! `events.event` notification, and is kept nowhere.

use std::collections::HashMap;
use std::sync::Mutex;

use rookery_kernel::conn::ConnHandle;
use rookery_kernel::{Call, Empty, Kernel, Level, Methods, RpcError};
use rookery_proto::jobs::{
    Delivered, EventNotice, EventPublish, EventSubscribe, EventSubscribed, EventUnsubscribe,
};
use serde_json::Value;

use crate::scoped;

struct Subscription {
    conn: ConnHandle,
    names: Vec<String>,
}

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
    let subscriptions = kernel.facility::<Subscriptions>();
    let all = subscriptions.0.lock().unwrap();
    let mut delivered = 0;
    for (id, s) in all.iter() {
        let reaches = user.is_none_or(|u| s.conn.caller().user().is_some_and(|me| me.id == u));
        if reaches && s.names.iter().any(|p| matches(p, name)) {
            s.conn.notify(
                "events.event",
                EventNotice {
                    subscription: id.clone(),
                    name: name.to_owned(),
                    payload: payload.clone(),
                },
            );
            delivered += 1;
        }
    }
    delivered
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
            Ok(Delivered {
                delivered: publish(&call.kernel, &name, &p.payload, p.user.as_deref()),
            })
        })
        .summary("Sends an event of the plugin to the connections subscribed now, at most once.")
        .level(Level::Plugin);
    methods
        .add("events.subscribe", |call: Call, p: EventSubscribe| async move {
            let conn = call.conn()?.clone();
            if p.names.is_empty() || p.names.len() > 100 || p.names.iter().any(|n| n.is_empty() || n.len() > 300) {
                return Err(RpcError::invalid("subscribe to 1 to 100 names, such as fleet/host.changed or fleet/*"));
            }
            let id = uuid::Uuid::now_v7().to_string();
            call.kernel.facility::<Subscriptions>().0.lock().unwrap().insert(id.clone(), Subscription { conn, names: p.names });
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
