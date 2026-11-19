//! Webhooks: `POST /hooks/<plugin>/<name>` for a webhook a plugin declares (`contributes.webhooks`),
//! which the plugin system sets with [`set_webhooks`] when it loads.
//!
//! The server verifies a delivery the way the webhook declares ([`verify`]), refuses replays by a hash of
//! what the signature covers, records the delivery, enqueues the webhook's job, `<plugin>/<job>`, with the
//! body, and answers `202` at once: no plugin code runs while the sender waits. The job is enqueued as the
//! plugin.
//!
//! Anyone may send requests, so refused ones and replays are counted per webhook, minute and status, and
//! written at most once a second each, not kept one by one. A webhook with `scheme: none`, which anyone
//! may trigger, takes [`NONE_PER_MINUTE`] deliveries a minute and enqueues while fewer than
//! [`NONE_QUEUED`] of its jobs wait; beyond that it answers `429` and enqueues nothing.
//!
//! The secret is a global secret of the plugin (`{ "secret": "<name>" }`, which its host half or an
//! admin with `webhooks.setSecret` sets), or a field of the credential of the plugin's global connection
//! of a type (`{ "connection": "<type>", "field": "webhookSecret" }`, the credential a JSON object).

mod verify;

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::DefaultBodyLimit;
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

pub use verify::{REPLAY_KEPT_SECS, Verified, sign, verify};

const DEFAULT_MAX_BYTES: u32 = 1 << 20;
const MAX_BYTES: u32 = 16 << 20;
/// Deliveries a minute a webhook with `scheme: none` takes.
pub const NONE_PER_MINUTE: u32 = 120;
/// Jobs of a webhook with `scheme: none` that may wait at once.
pub const NONE_QUEUED: i64 = 1000;
/// Headers never handed to the job.
const HIDDEN: &[&str] = &["authorization", "cookie", "proxy-authorization"];

/// The webhooks of each plugin, by their name after `<plugin>.`.
#[derive(Default)]
struct Hooks(RwLock<HashMap<String, Vec<(String, WebhookContribution)>>>);

/// The refusals of this minute, per webhook and status, until they are written.
#[derive(Default)]
struct Refused(Mutex<HashMap<(String, String, u16), Counted>>);

struct Counted {
    minute: i64,
    count: i64,
    written: Option<Instant>,
}

/// The deliveries of this minute of each webhook with `scheme: none`.
#[derive(Default)]
struct Unsigned(Mutex<HashMap<(String, String), (i64, u32)>>);

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
    // The body is read against the webhook's own maxBytes, so that a body too large is counted.
    Router::new().route(
        "/hooks/{plugin}/{name}",
        post(receive).layer(DefaultBodyLimit::disable()),
    )
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
    count(kernel, plugin, name, status, &reason, bytes).await;
    reply(status, json!({ "error": reason }))
}

/// Counts a refused request (or a replay) in its minute, and writes the count at most once a second.
async fn count(
    kernel: &Kernel,
    plugin: &str,
    name: &str,
    status: StatusCode,
    reason: &str,
    bytes: usize,
) {
    let now = Utc::now();
    let minute = now.timestamp() / 60;
    let (write, first) = {
        let counts = kernel.facility::<Refused>();
        let mut counts = counts.0.lock().unwrap();
        let c = counts
            .entry((plugin.to_owned(), name.to_owned(), status.as_u16()))
            .or_insert(Counted {
                minute,
                count: 0,
                written: None,
            });
        if c.minute != minute {
            *c = Counted {
                minute,
                count: 0,
                written: None,
            };
        }
        c.count += 1;
        let due = c
            .written
            .is_none_or(|w| w.elapsed() >= Duration::from_secs(1));
        if due {
            c.written = Some(Instant::now());
        }
        (due.then_some(c.count), c.count == 1)
    };
    if first {
        tracing::info!(webhook = %format!("{plugin}.{name}"), status = status.as_u16(), %reason, "a webhook refused a request; more this minute are counted");
    }
    let Some(count) = write else { return };
    let r = rookery_store::webhooks::Refusals {
        plugin: plugin.to_owned(),
        hook: name.to_owned(),
        minute: chrono::DateTime::from_timestamp(minute * 60, 0).unwrap_or(now),
        status: i32::from(status.as_u16()),
        count,
        reason: reason.to_owned(),
        bytes: i32::try_from(bytes).unwrap_or(i32::MAX),
    };
    if let Err(e) = kernel.store.webhooks().refused(&r).await {
        tracing::warn!(error = %e, "counting refused webhook requests failed");
    }
}

/// Whether a webhook with `scheme: none` may take one more delivery this minute.
fn unsigned_allowed(kernel: &Kernel, plugin: &str, name: &str) -> bool {
    let minute = Utc::now().timestamp() / 60;
    let taken = kernel.facility::<Unsigned>();
    let mut taken = taken.0.lock().unwrap();
    let t = taken
        .entry((plugin.to_owned(), name.to_owned()))
        .or_insert((minute, 0));
    if t.0 != minute {
        *t = (minute, 0);
    }
    t.1 += 1;
    t.1 <= NONE_PER_MINUTE
}

async fn receive(
    State(kernel): State<Kernel>,
    Path((plugin, name)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let id = format!("{plugin}.{name}");
    let Some(hook) = find(&kernel, &plugin, &name) else {
        return reply(
            StatusCode::NOT_FOUND,
            json!({ "error": format!("no webhook {id}") }),
        );
    };
    let limit = hook.max_bytes.unwrap_or(DEFAULT_MAX_BYTES) as usize;
    let declared = headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let body = match axum::body::to_bytes(body, limit).await {
        Ok(b) if declared <= limit => b,
        _ => {
            return refuse(
                &kernel,
                &plugin,
                &name,
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("the body is larger than the webhook's {limit} bytes"),
                declared.max(limit + 1),
            )
            .await;
        }
    };
    let bytes = body.len();
    if hook.verify.scheme == WebhookScheme::None && !unsigned_allowed(&kernel, &plugin, &name) {
        return refuse(
            &kernel,
            &plugin,
            &name,
            StatusCode::TOO_MANY_REQUESTS,
            format!("the webhook takes {NONE_PER_MINUTE} deliveries a minute"),
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
    if hook.verify.scheme == WebhookScheme::None {
        match kernel.store.jobs().queued(&kind, NONE_QUEUED).await {
            Ok(n) if n < NONE_QUEUED => {}
            Ok(_) => {
                return refuse(
                    &kernel,
                    &plugin,
                    &name,
                    StatusCode::TOO_MANY_REQUESTS,
                    format!("{NONE_QUEUED} jobs of the webhook wait already"),
                    bytes,
                )
                .await;
            }
            Err(e) => {
                tracing::error!(webhook = %id, error = %e, "counting a webhook's jobs failed");
                return reply(
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({ "error": "the delivery could not be kept: send it again" }),
                );
            }
        }
    }
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
        Ok(Accepted::Duplicate { job }) => {
            count(
                &kernel,
                &plugin,
                &name,
                StatusCode::OK,
                "a replay of an accepted delivery",
                bytes,
            )
            .await;
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
        count: None,
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
        let mut deliveries: Vec<DeliveryInfo> = kernel
            .store
            .webhooks()
            .list(Some(&plugin), Some(&name), 20)
            .await
            .map_err(rpc)?
            .iter()
            .map(delivery_info)
            .collect();
        let refusals = kernel
            .store
            .webhooks()
            .refusals(&plugin, &name, 20)
            .await
            .map_err(rpc)?;
        deliveries.extend(refusals.into_iter().map(|r| {
            DeliveryInfo {
                id: format!("{}:{}", r.minute.timestamp(), r.status),
                outcome: if r.status == 200 {
                    "duplicate"
                } else {
                    "refused"
                }
                .into(),
                count: Some(r.count),
                status: r.status,
                reason: Some(r.reason),
                job_id: None,
                bytes: r.bytes,
                received_at: time(r.minute),
            }
        }));
        deliveries.sort_by(|a, b| b.received_at.cmp(&a.received_at));
        deliveries.truncate(20);
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
            deliveries,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(WebhookList { webhooks: out })
}
