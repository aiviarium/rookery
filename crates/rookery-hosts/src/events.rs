//! What happens to hosts, told to clients: `hosts.changed` and `resources.changed` events, each
//! host's channel taken in on a durable topic `rookery/hosts/ID/channel`, and the messages a host keeps
//! as notifications to those who manage it.

use std::sync::Arc;

use rookery_api::Keep;
use rookery_kernel::{Call, Grant, Kernel};
use rookery_store::notify::New;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use crate::serve::{Event, Serve};

/// The durable topic a host's channel is taken in on, by the host's id.
pub fn channel_topic(id: &str) -> String {
    format!("rookery/hosts/{id}/channel")
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

async fn sees(kernel: &Kernel, caller: rookery_kernel::Caller, host: &str) -> bool {
    let call = Call::new(kernel.clone(), caller, None);
    call.is_admin()
        || rookery_facilities::access::holds(&call, &crate::uri(host), Grant::View)
            .await
            .unwrap_or(false)
}

async fn tell(kernel: &Kernel, serve: &Arc<Serve>, event: Event) {
    match event {
        Event::Host { name, change, from } => {
            let payload = json!({
                "host": name,
                "change": change,
                "from": from,
                "removed": change == "removed",
            });
            rookery_jobs::events::publish_where(kernel, "hosts.changed", &payload, |caller| {
                sees(kernel, caller, &name)
            })
            .await;
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
                "resources.changed",
                &payload,
                |caller| async move { caller.is_admin() },
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
            kernel.conns.notify_user(
                &user.to_string(),
                "notify.new",
                rookery_jobs::notify::info(sent),
            );
        }
    }
}
