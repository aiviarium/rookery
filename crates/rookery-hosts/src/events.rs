//! What happens to hosts, told to clients: `rookery/hosts.changed` and `rookery/resources.changed` events, each
//! host's channel taken in on a durable topic `rookery/hosts.ID.channel`, and the messages a host keeps
//! as notifications to those who manage it.

use std::sync::Arc;

use rookery_api::Keep;
use rookery_kernel::{Call, Grant, Kernel};
use rookery_store::notify::New;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use crate::serve::{Event, Serve};

/// The durable topic a host's channel is taken in on, by the host's id: one name of the plugin
/// `rookery`, as `topics.*` names a plugin's topics, with no slash in it.
pub fn channel_topic(id: &str) -> String {
    format!("rookery/hosts.{id}.channel")
}

/// Tells clients what happens, for as long as the server leads.
pub async fn forward(kernel: Kernel, serve: Arc<Serve>) {
    let mut events = serve.events.subscribe();
    loop {
        let event = tokio::select! {
            event = events.recv() => event,
            _ = kernel.shutdown.cancelled() => return,
        };
        match event {
            Ok(event) => tell(&kernel, &serve, event).await,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => return,
        }
    }
}

/// The server's events are its own plugin id's, as a plugin's are: `rookery/NAME`.
pub const HOSTS_CHANGED: &str = "rookery/hosts.changed";
pub const RESOURCES_CHANGED: &str = "rookery/resources.changed";
pub const ENV_CHANGED: &str = "rookery/env.changed";

/// Whether a connection hears of a host: an admin's, one with a grant on it, or a plugin's host half,
/// code an admin installed that keeps up with the hosts.
async fn sees(kernel: &Kernel, caller: rookery_kernel::Caller, host: &str) -> bool {
    if matches!(caller, rookery_kernel::Caller::Plugin(_)) {
        return true;
    }
    let call = Call::new(kernel.clone(), caller, None);
    call.is_admin()
        || rookery_facilities::access::holds(&call, &crate::uri(host), Grant::View)
            .await
            .unwrap_or(false)
}

async fn tell(kernel: &Kernel, serve: &Arc<Serve>, event: Event) {
    match event {
        Event::Host { name, change, from } => {
            let removed = change == "removed";
            // The host as hosts.get gives it, so a list keeps up without asking again.
            let info = match removed {
                true => None,
                false => crate::serve::api::host_info(serve, &name).await.ok(),
            };
            let payload = json!({
                "host": name,
                "info": info.clone(),
                "change": change,
                "from": from,
                "removed": removed,
            });
            rookery_jobs::events::publish_where(kernel, HOSTS_CHANGED, &payload, |caller| {
                sees(kernel, caller, &name)
            })
            .await;
            env_changed(kernel, &name, from.as_deref(), info).await;
        }
        Event::Resource {
            id,
            provisioner_name,
            kind,
            change,
            ..
        } => {
            let payload = json!({"resource": id, "provisioner": provisioner_name, "kind": kind, "change": change});
            rookery_jobs::events::publish_where(
                kernel,
                RESOURCES_CHANGED,
                &payload,
                |caller| async move {
                    caller.is_admin() || matches!(caller, rookery_kernel::Caller::Plugin(_))
                },
            )
            .await;
        }
        Event::Channel { host, message } => {
            let Some(remote) = serve.host(&host) else {
                return;
            };
            if remote.id.is_empty() {
                return;
            }
            let topic = channel_topic(&remote.id);
            let payload = json!({"host": host, "message": message});
            match kernel
                .store
                .topics()
                .publish(&topic, &payload, "system:hosts")
                .await
            {
                Ok(_) => rookery_jobs::topics::wake_topic(kernel, &topic),
                Err(e) => tracing::warn!(error = %e, %host, "publishing a host's message failed"),
            }
            if message.keep != Keep::None {
                notify_managers(kernel, &host, &message).await;
            }
        }
        Event::Inbox(_) | Event::Job(_) | Event::Kv { .. } => {}
    }
}

/// `rookery/env.changed`, with the host as `env.get` gives it to each subscriber who may see it. A
/// renamed host is removed under its old name.
async fn env_changed(
    kernel: &Kernel,
    name: &str,
    from: Option<&str>,
    info: Option<rookery_proto::hosts::HostInfo>,
) {
    if let Some(from) = from {
        let gone = json!(rookery_proto::env::EnvChanged {
            env: crate::uri(from),
            environment: None,
            removed: true,
        });
        rookery_jobs::events::publish_where(kernel, ENV_CHANGED, &gone, |caller| {
            sees(kernel, caller, name)
        })
        .await;
    }
    let env = crate::uri(name);
    rookery_jobs::events::publish_each(kernel, ENV_CHANGED, |caller| {
        let (env, info) = (env.clone(), info.clone());
        async move {
            let call = Call::new(kernel.clone(), caller, None);
            match info {
                Some(host) => {
                    let environment = crate::env::visible(&call, &host).await.ok().flatten()?;
                    serde_json::to_value(rookery_proto::env::EnvChanged {
                        env,
                        environment: Some(environment),
                        removed: false,
                    })
                    .ok()
                }
                // As hosts.changed: those who may still see it.
                None => sees(kernel, call.caller.clone(), name)
                    .await
                    .then(|| {
                        serde_json::to_value(rookery_proto::env::EnvChanged {
                            env,
                            environment: None,
                            removed: true,
                        })
                        .ok()
                    })
                    .flatten(),
            }
        }
    })
    .await;
}

/// The people who manage a host: its owner and those with manage on it, directly or by a group.
async fn managers(kernel: &Kernel, host: &str) -> Vec<Uuid> {
    let uri = crate::uri(host);
    sqlx::query_scalar(
        "SELECT user_id FROM rookery.owners WHERE resource = $1
         UNION SELECT substr(principal, 6)::uuid FROM rookery.grants WHERE resource = $1 AND level = 3 AND starts_with(principal, 'user:')
         UNION SELECT m.user_id FROM rookery.grants g JOIN rookery.group_members m ON 'group:' || m.group_id::text = g.principal
               WHERE g.resource = $1 AND g.level = 3",
    )
    .bind(&uri)
    .fetch_all(kernel.store.pool())
    .await
    .unwrap_or_default()
}

async fn notify_managers(kernel: &Kernel, host: &str, message: &rookery_api::Message) {
    let title = format!("{host}: {}", message.topic);
    let body = match &message.body {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Null => None,
        other => Some(other.to_string()),
    };
    let data = json!({"host": host, "message": message});
    for user in managers(kernel, host).await {
        let sent = kernel
            .store
            .notify()
            .send(New {
                user,
                kind: "rookery/host.message",
                title: &title,
                body: body.as_deref(),
                data: Some(&data),
                actions: None,
                sent_by: "system:hosts",
            })
            .await;
        if let Ok(Some(sent)) = sent {
            rookery_jobs::notify::announce(kernel, user, rookery_jobs::notify::info(sent)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_hosts_channel_is_a_topic_of_the_plugin_rookery() {
        // topics.* names a topic as PLUGIN/NAME, with no slash in NAME.
        let topic = super::channel_topic("0190f0a0b1c27d3e8f9a0b1c2d3e4f50");
        let name = topic.strip_prefix("rookery/").unwrap();
        assert!(!name.contains('/'), "{topic}");
    }
}
