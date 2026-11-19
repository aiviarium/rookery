//! Webhooks: `POST /hooks/<plugin>/<name>` for a webhook a plugin declares (`contributes.webhooks`),
//! which the plugin system sets with [`set_webhooks`] when it loads.
//!
//! The server verifies a delivery the way the webhook declares ([`verify`]), refuses replays where the
//! scheme gives a delivery id or a signed timestamp, records the delivery, enqueues the webhook's job,
//! `<plugin>/<job>`, with the body, and answers `202` at once: no plugin code runs while the sender
//! waits. The job is enqueued as the plugin.
//!
//! The secret is a global secret of the plugin (`{ "secret": "<name>" }`, which its host half or an
//! admin with `webhooks.setSecret` sets), or a field of the credential of the plugin's global connection
//! of a type (`{ "connection": "<type>", "field": "webhookSecret" }`, the credential a JSON object).

mod verify;

use std::collections::HashMap;
use std::sync::RwLock;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use chrono::Utc;
use rookery_kernel::{Call, Empty, Kernel, Level, Methods, RpcError};
use rookery_proto::schedules::{
    DeliveryInfo, SecretSource, WebhookContribution, WebhookInfo, WebhookList, WebhookScheme,
    WebhookSecretSet,
};
use rookery_store::jobs::NewJob;
use rookery_store::webhooks::{Accepted, Delivery};
use serde_json::{Map, Value, json};

use crate::{rpc, time};

pub use verify::{Verified, sign, verify};

const DEFAULT_MAX_BYTES: u32 = 1 << 20;
const MAX_BYTES: u32 = 16 << 20;
/// Headers never handed to the job.
const HIDDEN: &[&str] = &["authorization", "cookie", "proxy-authorization"];

/// The webhooks of each plugin, by their name after `<plugin>.`.
#[derive(Default)]
struct Hooks(RwLock<HashMap<String, Vec<(String, WebhookContribution)>>>);

/// Sets the webhooks a plugin declares, replacing those it had; refuses a declaration that cannot work.
pub fn set_webhooks(
    kernel: &Kernel,
    plugin: &str,
    hooks: Vec<WebhookContribution>,
) -> Result<(), RpcError> {
    let mut named = Vec::new();
    for w in hooks {
        let name =
            w.id.strip_prefix(&format!("{plugin}."))
                .or_else(|| w.id.strip_prefix(&format!("{plugin}/")))
                .unwrap_or(&w.id)
                .to_owned();
        if name.is_empty() || name.contains('/') || name.contains('.') {
            return Err(RpcError::invalid(format!(
                "the webhook {} needs a name of its own after {plugin}.",
                w.id
            )));
        }
        match (w.verify.scheme, &w.verify.secret) {
            (WebhookScheme::None, Some(_)) => {
                return Err(RpcError::invalid(format!(
                    "the webhook {}: the scheme none takes no secret",
                    w.id
                )));
            }
            (WebhookScheme::None, None) => {}
            (_, None) => {
                return Err(RpcError::invalid(format!(
                    "the webhook {}: verifying needs a secret, or \"scheme\": \"none\"",
                    w.id
                )));
            }
            _ => {}
        }
        if let Some(a) = &w.verify.algorithm
            && !matches!(a.as_str(), "sha256" | "sha1" | "sha512")
        {
            return Err(RpcError::invalid(format!(
                "the webhook {}: the algorithm {a:?} is none of sha256, sha1 and sha512",
                w.id
            )));
        }
        if w.max_bytes.is_some_and(|b| b > MAX_BYTES) {
            return Err(RpcError::invalid(format!(
                "the webhook {}: maxBytes is at most {MAX_BYTES}",
                w.id
            )));
        }
        named.push((name, w));
    }
    let all = kernel.facility::<Hooks>();
    let mut all = all.0.write().unwrap();
    if named.is_empty() {
        all.remove(plugin);
    } else {
        all.insert(plugin.to_owned(), named);
    }
    Ok(())
}

fn find(kernel: &Kernel, plugin: &str, name: &str) -> Option<WebhookContribution> {
    let all = kernel.facility::<Hooks>();
    let all = all.0.read().unwrap();
    all.get(plugin)?
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, w)| w.clone())
}

pub fn register(methods: &mut Methods) {
    methods
        .add("webhooks.list", |call: Call, _: Empty| async move {
            list(&call.kernel).await
        })
        .summary("The webhooks plugins declare, whether each is ready, and their last deliveries.")
        .level(Level::Admin);
    methods
        .add(
            "webhooks.setSecret",
            |call: Call, p: WebhookSecretSet| async move {
                let (plugin, name) =
                    p.id.split_once(['.', '/'])
                        .ok_or_else(|| RpcError::invalid("a webhook is <plugin>.<name>"))?;
                let hook = find(&call.kernel, plugin, name)
                    .ok_or_else(|| RpcError::not_found(format!("no webhook {}", p.id)))?;
                let Some(SecretSource::Secret { secret }) = &hook.verify.secret else {
                    return Err(RpcError::invalid(format!(
                        "the webhook {} takes its secret from no secret of the plugin",
                        p.id
                    )));
                };
                if p.secret.is_empty() {
                    return Err(RpcError::invalid("the secret is empty"));
                }
                call.store()
                    .secrets()
                    .set(
                        plugin,
                        secret,
                        rookery_store::secrets::Scope::Global,
                        p.secret.as_bytes(),
                    )
                    .await
                    .map_err(rpc)?;
                call.audit(
                    "webhooks.setSecret",
                    Some(&format!("plugin://{plugin}")),
                    json!({ "webhook": p.id }),
                )
                .await;
                Ok(Empty {})
            },
        )
        .summary("Sets the secret a webhook verifies its deliveries with.")
        .level(Level::Admin);
}

/// The route of the webhooks, for the edge.
pub fn routes() -> Router<Kernel> {
    Router::new().route("/hooks/{plugin}/{name}", post(receive))
}

/// The secret a webhook verifies with, if it is set.
async fn secret(
    kernel: &Kernel,
    plugin: &str,
    hook: &WebhookContribution,
) -> Result<Option<Vec<u8>>, String> {
    match &hook.verify.secret {
        None => Ok(None),
        Some(SecretSource::Secret { secret }) => kernel
            .store
            .secrets()
            .get(plugin, secret, rookery_store::secrets::Scope::Global)
            .await
            .map_err(|e| e.to_string()),
        Some(SecretSource::Connection { connection, field }) => {
            let field = field.as_deref().unwrap_or("webhookSecret");
            let all = kernel
                .store
                .connections()
                .list(Some(connection), None)
                .await
                .map_err(|e| e.to_string())?;
            let Some(c) = all.into_iter().find(|c| c.user_id.is_none()) else {
                return Ok(None);
            };
            let Some(credential) = kernel
                .store
                .connections()
                .credential(c.id)
                .await
                .map_err(|e| e.to_string())?
            else {
                return Ok(None);
            };
            let value: Value = serde_json::from_slice(&credential).unwrap_or(Value::Null);
            Ok(value
                .get(field)
                .and_then(Value::as_str)
                .map(|s| s.as_bytes().to_vec()))
        }
    }
}

fn reply(status: StatusCode, body: Value) -> Response {
    (status, axum::Json(body)).into_response()
}

async fn refuse(
    kernel: &Kernel,
    plugin: &str,
    name: &str,
    status: StatusCode,
    reason: String,
    bytes: usize,
) -> Response {
    tracing::info!(webhook = %format!("{plugin}.{name}"), %reason, "a webhook delivery was refused");
    if let Err(e) = kernel
        .store
        .webhooks()
        .refuse(plugin, name, status.as_u16(), &reason, bytes)
        .await
    {
        tracing::warn!(error = %e, "recording a refused webhook delivery failed");
    }
    reply(status, json!({ "error": reason }))
}

async fn receive(
    State(kernel): State<Kernel>,
    Path((plugin, name)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let id = format!("{plugin}.{name}");
    let Some(hook) = find(&kernel, &plugin, &name) else {
        return reply(
            StatusCode::NOT_FOUND,
            json!({ "error": format!("no webhook {id}") }),
        );
    };
    let bytes = body.len();
    if bytes > hook.max_bytes.unwrap_or(DEFAULT_MAX_BYTES) as usize {
        return refuse(
            &kernel,
            &plugin,
            &name,
            StatusCode::PAYLOAD_TOO_LARGE,
            "the body is too large".into(),
            bytes,
        )
        .await;
    }
    let secret = match secret(&kernel, &plugin, &hook).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(webhook = %id, error = %e, "reading the webhook's secret failed");
            return refuse(
                &kernel,
                &plugin,
                &name,
                StatusCode::SERVICE_UNAVAILABLE,
                "the webhook's secret cannot be read".into(),
                bytes,
            )
            .await;
        }
    };
    if hook.verify.scheme != WebhookScheme::None && secret.is_none() {
        return refuse(
            &kernel,
            &plugin,
            &name,
            StatusCode::SERVICE_UNAVAILABLE,
            "the webhook's secret is not set".into(),
            bytes,
        )
        .await;
    }
    let verified = match verify(
        &hook.verify,
        secret.as_deref(),
        &headers,
        &body,
        Utc::now().timestamp(),
    ) {
        Ok(v) => v,
        Err(reason) => {
            return refuse(
                &kernel,
                &plugin,
                &name,
                StatusCode::UNAUTHORIZED,
                reason,
                bytes,
            )
            .await;
        }
    };
    // The job gets the body, read as JSON when it says it is, and the headers worth reading.
    let mut hdrs = Map::new();
    for (k, v) in &headers {
        let k = k.as_str().to_ascii_lowercase();
        if HIDDEN.contains(&k.as_str()) {
            continue;
        }
        if let Ok(v) = v.to_str() {
            hdrs.insert(k, Value::String(v.to_string()));
        }
    }
    let text = String::from_utf8_lossy(&body).into_owned();
    let is_json = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.contains("json"));
    let parsed = if is_json {
        serde_json::from_slice::<Value>(&body).ok()
    } else {
        None
    };
    let params = json!({
        "webhook": id,
        "deliveryId": verified.delivery_id,
        "receivedAt": time(Utc::now()),
        "headers": hdrs,
        "body": parsed.clone().unwrap_or(Value::String(text.clone())),
        "text": if parsed.is_some() { Value::Null } else { Value::String(text) },
    });
    let job_name = hook
        .job
        .strip_prefix(&format!("{plugin}/"))
        .unwrap_or(&hook.job);
    let kind = format!("{plugin}/{job_name}");
    let by = format!("plugin:{plugin}");
    let job = NewJob {
        kind: &kind,
        params: &params,
        max_attempts: 1,
        run_after: None,
        subject: None,
        enqueued_by: &by,
    };
    match kernel
        .store
        .webhooks()
        .accept(&plugin, &name, verified.replay_key.as_deref(), bytes, &job)
        .await
    {
        Ok(Accepted::New { job, .. }) => {
            rookery_jobs::wake(&kernel);
            reply(StatusCode::ACCEPTED, json!({ "job": job.id }))
        }
        Ok(Accepted::Duplicate { job, .. }) => {
            reply(StatusCode::OK, json!({ "duplicate": true, "job": job }))
        }
        Err(e) => {
            tracing::error!(webhook = %id, error = %e, "accepting a webhook delivery failed");
            reply(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({ "error": "the delivery could not be kept: send it again" }),
            )
        }
    }
}

fn delivery_info(d: &Delivery) -> DeliveryInfo {
    DeliveryInfo {
        id: d.id.to_string(),
        outcome: d.outcome.clone(),
        status: d.status,
        reason: d.reason.clone(),
        job_id: d.job_id.map(|j| j.to_string()),
        bytes: d.bytes,
        received_at: time(d.received_at),
    }
}

async fn list(kernel: &Kernel) -> Result<WebhookList, RpcError> {
    let declared: Vec<(String, String, WebhookContribution)> = {
        let all = kernel.facility::<Hooks>();
        let all = all.0.read().unwrap();
        all.iter()
            .flat_map(|(p, hooks)| hooks.iter().map(|(n, w)| (p.clone(), n.clone(), w.clone())))
            .collect()
    };
    let mut out = Vec::new();
    for (plugin, name, w) in declared {
        let deliveries = kernel
            .store
            .webhooks()
            .list(Some(&plugin), Some(&name), 20)
            .await
            .map_err(rpc)?;
        let ready = w.verify.scheme == WebhookScheme::None
            || secret(kernel, &plugin, &w).await.ok().flatten().is_some();
        out.push(WebhookInfo {
            plugin: plugin.clone(),
            id: w.id.clone(),
            title: w.title.clone(),
            path: format!("/hooks/{plugin}/{name}"),
            job: w.job.clone(),
            scheme: serde_json::to_value(w.verify.scheme)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
            secret: w.verify.secret.as_ref().map(|s| match s {
                SecretSource::Secret { secret } => format!("secret {secret}"),
                SecretSource::Connection { connection, .. } => format!("connection {connection}"),
            }),
            ready,
            deliveries: deliveries.iter().map(delivery_info).collect(),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(WebhookList { webhooks: out })
}
