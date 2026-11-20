//! Rust SDK for Rookery app services.
//!
//! An app service receives calls from Rookery over `POST /rpc` (authenticated with its inbound
//! `ROOKERY_SERVICE_SECRET`, carrying a server-minted `ctx.callId`) and calls Rookery back over
//! `POST <ROOKERY_URL>/services/rpc` (authenticated with its distinct `ROOKERY_CALLBACK_SECRET`).
//!
//! - Calls made through [`Ctx`] include `callId`, so Rookery runs person-level methods as the
//!   person the invocation serves and plugin-level methods (`connections.resolve`, `audit.record`)
//!   at the plugin's own level bound to that person.
//! - Calls made through [`Client`] omit `callId` and run at the plugin's own level (its own
//!   collections, extensions, global connections of its declared types).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::Stream;
use rookery_proto::data::{EntityInfo, EntityTextList};
use rookery_proto::facilities::{ConnectionInfo, ConnectionResolved};
use rookery_proto::methods::Grant;
pub use rookery_proto::{ErrorCode, Principal, PrincipalKind, RpcError, User};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub use rookery_proto;
pub use tokio_util::sync::CancellationToken as CancelToken;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type BoxStream<T> = Pin<Box<dyn Stream<Item = T> + Send>>;
pub type Result<T> = std::result::Result<T, RpcError>;

const MAX_STREAM_LINE: usize = 1024 * 1024;
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Configuration for a Rookery app service.
#[derive(Clone)]
pub struct Config {
    /// Base URL of the Rookery server (e.g. `http://127.0.0.1:4100`).
    pub rookery_url: String,
    /// Socket address to bind the app service HTTP server on (e.g. `127.0.0.1:8080`).
    pub listen: String,
    /// Plugin id that owns this app service (e.g. `ai`).
    pub plugin_id: String,
    /// Service id (e.g. `router` or `ai/router`).
    pub service_id: String,
    /// Inbound bearer secret Rookery presents when calling `POST /rpc`.
    pub inbound_secret: String,
    /// Distinct callback bearer secret this service presents on `POST /services/rpc`.
    pub callback_secret: String,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("rookery_url", &self.rookery_url)
            .field("listen", &self.listen)
            .field("plugin_id", &self.plugin_id)
            .field("service_id", &self.service_id)
            .field("inbound_secret", &"[redacted]")
            .field("callback_secret", &"[redacted]")
            .finish()
    }
}

impl Config {
    /// Reads configuration from standard Rookery app-service environment variables:
    /// - `ROOKERY_URL`
    /// - `ROOKERY_SERVICE_ID` (e.g. `ai/router` or `router`)
    /// - `ROOKERY_PLUGIN_ID` (optional when `ROOKERY_SERVICE_ID` has `<plugin>/<service>`)
    /// - `ROOKERY_SERVICE_SECRET` (inbound bearer token)
    /// - `ROOKERY_CALLBACK_SECRET` (distinct callback bearer token for `/services/rpc`)
    /// - `ROOKERY_SERVICE_LISTEN` / `ROOKERY_LISTEN` (defaults to `127.0.0.1:8080`)
    pub fn from_env() -> Result<Self> {
        let rookery_url = std::env::var("ROOKERY_URL").unwrap_or_default();
        let service_id = std::env::var("ROOKERY_SERVICE_ID").unwrap_or_default();
        let plugin_id = std::env::var("ROOKERY_PLUGIN_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| service_id.split_once('/').map(|(p, _)| p.to_owned()))
            .unwrap_or_default();
        let inbound_secret = std::env::var("ROOKERY_SERVICE_SECRET").unwrap_or_default();
        let callback_secret = std::env::var("ROOKERY_CALLBACK_SECRET").unwrap_or_default();
        let listen = std::env::var("ROOKERY_SERVICE_LISTEN")
            .or_else(|_| std::env::var("ROOKERY_LISTEN"))
            .unwrap_or_else(|_| "127.0.0.1:8080".into());
        let cfg = Self {
            rookery_url,
            listen,
            plugin_id,
            service_id,
            inbound_secret,
            callback_secret,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if self.rookery_url.trim().is_empty() {
            return Err(RpcError::invalid("ROOKERY_URL is required"));
        }
        if self.service_id.trim().is_empty() {
            return Err(RpcError::invalid("ROOKERY_SERVICE_ID is required"));
        }
        if self.inbound_secret.trim().is_empty() {
            return Err(RpcError::invalid("ROOKERY_SERVICE_SECRET is required"));
        }
        if self.callback_secret.trim().is_empty() {
            return Err(RpcError::invalid("ROOKERY_CALLBACK_SECRET is required"));
        }
        if self.inbound_secret == self.callback_secret {
            return Err(RpcError::invalid(
                "ROOKERY_CALLBACK_SECRET must differ from ROOKERY_SERVICE_SECRET",
            ));
        }
        Ok(())
    }
}

/// Constant-time equality for bearer secrets.
fn secret_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Plugin-level callback client for Rookery (`POST <rookery_url>/services/rpc`).
///
/// Calls made directly on `Client` omit `callId` and run at the plugin's own level.
#[derive(Clone)]
pub struct Client {
    endpoint: Arc<String>,
    callback_secret: Arc<String>,
    plugin_id: Arc<String>,
    service_id: Arc<String>,
}

impl Client {
    pub fn new(
        rookery_url: impl Into<String>,
        callback_secret: impl Into<String>,
        plugin_id: impl Into<String>,
        service_id: impl Into<String>,
    ) -> Self {
        let base = rookery_url.into();
        let endpoint = format!("{}/services/rpc", base.trim_end_matches('/'));
        Self {
            endpoint: Arc::new(endpoint),
            callback_secret: Arc::new(callback_secret.into()),
            plugin_id: Arc::new(plugin_id.into()),
            service_id: Arc::new(service_id.into()),
        }
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    pub fn principal(&self) -> Principal {
        Principal {
            kind: PrincipalKind::Plugin,
            id: self.service_id.as_ref().clone(),
        }
    }

    /// Calls a Rookery method at the plugin's own level (without `callId`).
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_raw(method, params, None, DEFAULT_CALL_TIMEOUT)
            .await
    }

    /// Streams a Rookery method (`services.stream`) at the plugin's own level.
    pub async fn stream(&self, method: &str, params: Value) -> Result<BoxStream<Result<Value>>> {
        self.stream_raw(method, params, None, Duration::from_secs(300))
            .await
    }

    pub async fn call_raw(
        &self,
        method: &str,
        params: Value,
        call_id: Option<&str>,
        timeout: Duration,
    ) -> Result<Value> {
        let endpoint = self.endpoint.as_ref().clone();
        let secret = self.callback_secret.as_ref().clone();
        let mut body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        if let Some(cid) = call_id {
            body["callId"] = Value::String(cid.to_owned());
        }
        let payload = body.to_string();
        let answer = tokio::task::spawn_blocking(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(timeout))
                .http_status_as_error(false)
                .build()
                .into();
            let mut response = agent
                .post(&endpoint)
                .header("content-type", "application/json")
                .header("authorization", &format!("Bearer {secret}"))
                .send(payload.as_bytes())
                .map_err(|e| RpcError::unavailable(format!("{endpoint}: {e}")))?;
            let status = response.status().as_u16();
            let text = response
                .body_mut()
                .read_to_string()
                .map_err(|e| RpcError::unavailable(format!("{endpoint}: {e}")))?;
            let parsed: Value = serde_json::from_str(&text).map_err(|e| {
                RpcError::unavailable(format!(
                    "{endpoint} answered HTTP {status} with non-JSON: {e}"
                ))
            })?;
            Ok::<_, RpcError>((status, parsed))
        })
        .await
        .map_err(|e| RpcError::internal(e.to_string()))??;
        let (status, parsed) = answer;
        if let Some(error) = parsed.get("error") {
            let err = serde_json::from_value::<RpcError>(error.clone())
                .unwrap_or_else(|_| RpcError::internal(error.to_string()));
            return Err(err);
        }
        if !(200..300).contains(&status) {
            return Err(RpcError::unavailable(format!(
                "Rookery callback returned HTTP {status}"
            )));
        }
        Ok(parsed.get("result").cloned().unwrap_or(Value::Null))
    }

    pub async fn stream_raw(
        &self,
        method: &str,
        params: Value,
        call_id: Option<&str>,
        timeout: Duration,
    ) -> Result<BoxStream<Result<Value>>> {
        let endpoint = self.endpoint.as_ref().clone();
        let secret = self.callback_secret.as_ref().clone();
        let mut body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
            "stream": true,
        });
        if let Some(cid) = call_id {
            body["callId"] = Value::String(cid.to_owned());
        }
        let payload = body.to_string();
        let (tx, rx) = mpsc::channel::<Result<Value>>(16);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Result<()>>();
        tokio::task::spawn_blocking(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(timeout))
                .http_status_as_error(false)
                .build()
                .into();
            let mut response = match agent
                .post(&endpoint)
                .header("content-type", "application/json")
                .header("accept", "application/x-ndjson, application/json")
                .header("authorization", &format!("Bearer {secret}"))
                .send(payload.as_bytes())
            {
                Ok(resp) => resp,
                Err(e) => {
                    let _ = ready_tx.send(Err(RpcError::unavailable(format!("{endpoint}: {e}"))));
                    return;
                }
            };
            let status = response.status().as_u16();
            let is_ndjson = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|ct| ct.contains("ndjson"));
            if !is_ndjson {
                let text = response.body_mut().read_to_string().unwrap_or_default();
                let err = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| v.get("error").cloned())
                    .and_then(|e| serde_json::from_value::<RpcError>(e).ok())
                    .unwrap_or_else(|| {
                        RpcError::unavailable(format!(
                            "Rookery callback stream returned HTTP {status}"
                        ))
                    });
                let _ = ready_tx.send(Err(err));
                return;
            }
            let _ = ready_tx.send(Ok(()));
            let mut reader = response.body_mut().as_reader();
            let mut pending = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                if tx.is_closed() {
                    return;
                }
                match std::io::Read::read(&mut reader, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        pending.extend_from_slice(&buf[..n]);
                        while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                            let line: Vec<u8> = pending.drain(..=end).collect();
                            let Ok(msg) = serde_json::from_slice::<Value>(&line[..end]) else {
                                continue;
                            };
                            if let Some(error) = msg.get("error") {
                                let err = serde_json::from_value::<RpcError>(error.clone())
                                    .unwrap_or_else(|_| RpcError::internal(error.to_string()));
                                let _ = tx.blocking_send(Err(err));
                                return;
                            }
                            let Some(item) = msg.get("result").cloned() else {
                                return;
                            };
                            if tx.blocking_send(Ok(item)).is_err() {
                                return;
                            }
                        }
                        if pending.len() > MAX_STREAM_LINE {
                            let _ = tx.blocking_send(Err(RpcError::unavailable(
                                "callback stream line exceeded limit",
                            )));
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .blocking_send(Err(RpcError::unavailable(format!("{endpoint}: {e}"))));
                        return;
                    }
                }
            }
        });
        ready_rx
            .await
            .map_err(|_| RpcError::unavailable("callback stream worker stopped"))??;
        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(Box::pin(stream))
    }

    pub fn data(&self) -> DataClient<'_> {
        DataClient {
            client: self,
            call_id: None,
            plugin: if self.plugin_id.is_empty() {
                None
            } else {
                Some(&self.plugin_id)
            },
            space: None,
            timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    pub fn connections(&self) -> ConnectionsClient<'_> {
        ConnectionsClient {
            client: self,
            call_id: None,
            timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    pub fn entities(&self) -> EntitiesClient<'_> {
        EntitiesClient {
            client: self,
            call_id: None,
            timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    pub fn extensions(&self) -> ExtensionsClient<'_> {
        ExtensionsClient {
            client: self,
            call_id: None,
            timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    pub fn services(&self) -> ServicesClient<'_> {
        ServicesClient {
            client: self,
            call_id: None,
            timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    pub fn audit(&self) -> AuditClient<'_> {
        AuditClient {
            client: self,
            call_id: None,
            timeout: DEFAULT_CALL_TIMEOUT,
        }
    }
}

/// Per-invocation context passed to app-service handlers.
///
/// Every callback made through `Ctx` carries the server-minted `callId` from this invocation so
/// Rookery enforces the served person's permissions (or plugin level when serving no person).
#[derive(Clone)]
pub struct Ctx {
    client: Client,
    call_id: String,
    user: Option<User>,
    principal: Principal,
    space: Option<String>,
    deadline_ms: Option<i64>,
    cancel: CancellationToken,
}

impl Ctx {
    /// Plugin-level callback client (calls without `callId`, at the plugin's own level).
    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    pub fn user(&self) -> Option<&User> {
        self.user.as_ref()
    }

    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    pub fn space(&self) -> Option<&str> {
        self.space.as_deref()
    }

    pub fn deadline_ms(&self) -> Option<i64> {
        self.deadline_ms
    }

    pub fn time_left(&self) -> Duration {
        let Some(deadline_ms) = self.deadline_ms else {
            return DEFAULT_CALL_TIMEOUT;
        };
        let now = chrono::Utc::now().timestamp_millis();
        let left = (deadline_ms - now).max(1) as u64;
        Duration::from_millis(left)
    }

    pub fn cancellation(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub async fn cancelled(&self) {
        self.cancel.cancelled().await;
    }

    /// Calls a Rookery method bound to this invocation's `callId`.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.client
            .call_raw(method, params, Some(&self.call_id), self.time_left())
            .await
    }

    /// Streams a Rookery method (`services.stream`) bound to this invocation's `callId`.
    pub async fn stream(&self, method: &str, params: Value) -> Result<BoxStream<Result<Value>>> {
        self.client
            .stream_raw(method, params, Some(&self.call_id), self.time_left())
            .await
    }

    pub fn data(&self) -> DataClient<'_> {
        DataClient {
            client: &self.client,
            call_id: Some(&self.call_id),
            plugin: if self.client.plugin_id.is_empty() {
                None
            } else {
                Some(&self.client.plugin_id)
            },
            space: self.space.as_deref(),
            timeout: self.time_left(),
        }
    }

    pub fn connections(&self) -> ConnectionsClient<'_> {
        ConnectionsClient {
            client: &self.client,
            call_id: Some(&self.call_id),
            timeout: self.time_left(),
        }
    }

    pub fn entities(&self) -> EntitiesClient<'_> {
        EntitiesClient {
            client: &self.client,
            call_id: Some(&self.call_id),
            timeout: self.time_left(),
        }
    }

    pub fn access(&self) -> AccessClient<'_> {
        AccessClient {
            client: &self.client,
            call_id: Some(&self.call_id),
            timeout: self.time_left(),
        }
    }

    pub fn extensions(&self) -> ExtensionsClient<'_> {
        ExtensionsClient {
            client: &self.client,
            call_id: Some(&self.call_id),
            timeout: self.time_left(),
        }
    }

    pub fn services(&self) -> ServicesClient<'_> {
        ServicesClient {
            client: &self.client,
            call_id: Some(&self.call_id),
            timeout: self.time_left(),
        }
    }

    pub fn audit(&self) -> AuditClient<'_> {
        AuditClient {
            client: &self.client,
            call_id: Some(&self.call_id),
            timeout: self.time_left(),
        }
    }
}

pub struct DataClient<'a> {
    client: &'a Client,
    call_id: Option<&'a str>,
    plugin: Option<&'a str>,
    space: Option<&'a str>,
    timeout: Duration,
}

impl<'a> DataClient<'a> {
    pub fn plugin(mut self, plugin: &'a str) -> Self {
        self.plugin = Some(plugin);
        self
    }

    pub fn space(mut self, space: &'a str) -> Self {
        self.space = Some(space);
        self
    }

    fn with_target(&self, collection: &str, mut params: Value) -> Value {
        if let Some(obj) = params.as_object_mut() {
            obj.insert("collection".into(), Value::String(collection.to_owned()));
            if let Some(plugin) = self.plugin {
                obj.entry("plugin")
                    .or_insert_with(|| Value::String(plugin.to_owned()));
            }
            if let Some(space) = self.space {
                obj.entry("space")
                    .or_insert_with(|| Value::String(space.to_owned()));
            }
        }
        params
    }

    pub async fn find(
        &self,
        collection: &str,
        filter: Option<Value>,
        limit: Option<usize>,
    ) -> Result<Vec<Value>> {
        let mut query = json!({});
        if let Some(f) = filter {
            query["where"] = f;
        }
        if let Some(l) = limit {
            query["limit"] = json!(l);
        }
        let v = self
            .client
            .call_raw(
                "data.find",
                self.with_target(collection, json!({ "query": query })),
                self.call_id,
                self.timeout,
            )
            .await?;
        Ok(v.get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub async fn get(&self, collection: &str, id: &str) -> Result<Option<Value>> {
        let v = self
            .client
            .call_raw(
                "data.get",
                self.with_target(collection, json!({ "id": id })),
                self.call_id,
                self.timeout,
            )
            .await?;
        Ok(v.get("row").cloned().filter(|r| !r.is_null()))
    }

    pub async fn insert(&self, collection: &str, values: Value) -> Result<Value> {
        let v = self
            .client
            .call_raw(
                "data.insert",
                self.with_target(collection, json!({ "values": values })),
                self.call_id,
                self.timeout,
            )
            .await?;
        Ok(v.get("row").cloned().unwrap_or(v))
    }

    pub async fn upsert(&self, collection: &str, values: Value, on: &[&str]) -> Result<Value> {
        let v = self
            .client
            .call_raw(
                "data.upsert",
                self.with_target(collection, json!({ "values": values, "on": on })),
                self.call_id,
                self.timeout,
            )
            .await?;
        Ok(v.get("row").cloned().unwrap_or(v))
    }

    pub async fn update(
        &self,
        collection: &str,
        id: &str,
        patch: Value,
        rev: Option<i64>,
    ) -> Result<Value> {
        let mut params = json!({ "id": id, "patch": patch });
        if let Some(r) = rev {
            params["rev"] = json!(r);
        }
        let v = self
            .client
            .call_raw(
                "data.update",
                self.with_target(collection, params),
                self.call_id,
                self.timeout,
            )
            .await?;
        Ok(v.get("row").cloned().unwrap_or(v))
    }

    pub async fn delete(&self, collection: &str, id: &str) -> Result<()> {
        self.client
            .call_raw(
                "data.delete",
                self.with_target(collection, json!({ "id": id })),
                self.call_id,
                self.timeout,
            )
            .await?;
        Ok(())
    }

    pub async fn aggregate(&self, collection: &str, spec: Value) -> Result<Vec<Value>> {
        let v = self
            .client
            .call_raw(
                "data.aggregate",
                self.with_target(collection, spec),
                self.call_id,
                self.timeout,
            )
            .await?;
        Ok(v.get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }
}

pub struct ConnectionsClient<'a> {
    client: &'a Client,
    call_id: Option<&'a str>,
    timeout: Duration,
}

impl<'a> ConnectionsClient<'a> {
    /// Resolves a connection of this plugin's own declared type, including its sealed credential.
    pub async fn resolve(&self, id: &str) -> Result<Option<ConnectionResolved>> {
        match self
            .client
            .call_raw(
                "connections.resolve",
                json!({ "id": id }),
                self.call_id,
                self.timeout,
            )
            .await
        {
            Ok(v) => serde_json::from_value(v)
                .map(Some)
                .map_err(|e| RpcError::internal(format!("bad connections.resolve: {e}"))),
            Err(e) if e.error_code() == ErrorCode::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Reads connection metadata without secrets (works for connections of any type visible to the caller).
    pub async fn get(&self, id: &str) -> Result<Option<ConnectionInfo>> {
        match self
            .client
            .call_raw(
                "connections.get",
                json!({ "id": id }),
                self.call_id,
                self.timeout,
            )
            .await
        {
            Ok(v) => serde_json::from_value(v)
                .map(Some)
                .map_err(|e| RpcError::internal(format!("bad connections.get: {e}"))),
            Err(e) if e.error_code() == ErrorCode::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub async fn list(&self, kind: Option<&str>) -> Result<Vec<ConnectionInfo>> {
        let params = match kind {
            Some(k) => json!({ "type": k }),
            None => json!({}),
        };
        let v = self
            .client
            .call_raw("connections.list", params, self.call_id, self.timeout)
            .await?;
        let list = v
            .get("connections")
            .cloned()
            .unwrap_or_else(|| Value::Array(vec![]));
        serde_json::from_value(list)
            .map_err(|e| RpcError::internal(format!("bad connections.list: {e}")))
    }
}

pub struct EntitiesClient<'a> {
    client: &'a Client,
    call_id: Option<&'a str>,
    timeout: Duration,
}

impl<'a> EntitiesClient<'a> {
    pub async fn text(&self, uris: &[&str], max_chars: Option<u32>) -> Result<EntityTextList> {
        let mut params = json!({ "uris": uris });
        if let Some(max) = max_chars {
            params["maxChars"] = json!(max);
        }
        let v = self
            .client
            .call_raw("entities.text", params, self.call_id, self.timeout)
            .await?;
        serde_json::from_value(v).map_err(|e| RpcError::internal(format!("bad entities.text: {e}")))
    }

    pub async fn resolve(&self, uri: &str) -> Result<Option<EntityInfo>> {
        let v = self
            .client
            .call_raw(
                "entities.resolve",
                json!({ "uri": uri }),
                self.call_id,
                self.timeout,
            )
            .await?;
        if v.is_null() {
            return Ok(None);
        }
        serde_json::from_value(v)
            .map(Some)
            .map_err(|e| RpcError::internal(format!("bad entities.resolve: {e}")))
    }

    pub async fn search(&self, text: &str, limit: Option<u32>) -> Result<Vec<EntityInfo>> {
        let mut params = json!({ "text": text });
        if let Some(l) = limit {
            params["limit"] = json!(l);
        }
        let v = self
            .client
            .call_raw("entities.search", params, self.call_id, self.timeout)
            .await?;
        let list = v
            .get("entities")
            .cloned()
            .unwrap_or_else(|| Value::Array(vec![]));
        serde_json::from_value(list)
            .map_err(|e| RpcError::internal(format!("bad entities.search: {e}")))
    }
}

pub struct AccessClient<'a> {
    client: &'a Client,
    call_id: Option<&'a str>,
    timeout: Duration,
}

impl<'a> AccessClient<'a> {
    pub async fn filter(&self, resources: &[&str], level: Grant) -> Result<Vec<String>> {
        let v = self
            .client
            .call_raw(
                "access.filter",
                json!({ "resources": resources, "level": level }),
                self.call_id,
                self.timeout,
            )
            .await?;
        let list = v
            .get("resources")
            .cloned()
            .unwrap_or_else(|| Value::Array(vec![]));
        serde_json::from_value(list)
            .map_err(|e| RpcError::internal(format!("bad access.filter: {e}")))
    }

    pub async fn require(&self, resource: &str, level: Grant) -> Result<()> {
        self.client
            .call_raw(
                "access.require",
                json!({ "resource": resource, "level": level }),
                self.call_id,
                self.timeout,
            )
            .await?;
        Ok(())
    }

    pub async fn level(&self, resource: &str) -> Result<Option<Grant>> {
        let v = self
            .client
            .call_raw(
                "access.level",
                json!({ "resource": resource }),
                self.call_id,
                self.timeout,
            )
            .await?;
        let lvl = v.get("level").cloned().unwrap_or(Value::Null);
        if lvl.is_null() {
            return Ok(None);
        }
        serde_json::from_value(lvl)
            .map(Some)
            .map_err(|e| RpcError::internal(format!("bad access.level: {e}")))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtensionItem {
    pub plugin: String,
    pub value: Value,
    #[serde(default)]
    pub service: Option<String>,
}

pub struct ExtensionsClient<'a> {
    client: &'a Client,
    call_id: Option<&'a str>,
    timeout: Duration,
}

impl<'a> ExtensionsClient<'a> {
    pub async fn list(&self, point: &str) -> Result<Vec<ExtensionItem>> {
        let v = self
            .client
            .call_raw(
                "extensions.list",
                json!({ "point": point }),
                self.call_id,
                self.timeout,
            )
            .await?;
        let list = v
            .get("contributions")
            .cloned()
            .unwrap_or_else(|| Value::Array(vec![]));
        serde_json::from_value(list)
            .map_err(|e| RpcError::internal(format!("bad extensions.list: {e}")))
    }
}

pub struct ServicesClient<'a> {
    client: &'a Client,
    call_id: Option<&'a str>,
    timeout: Duration,
}

impl<'a> ServicesClient<'a> {
    pub async fn call(
        &self,
        plugin: &str,
        service: &str,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        self.client
            .call_raw(
                "services.call",
                json!({
                    "plugin": plugin,
                    "service": service,
                    "method": method,
                    "params": params,
                }),
                self.call_id,
                self.timeout,
            )
            .await
    }

    pub async fn stream(
        &self,
        plugin: &str,
        service: &str,
        method: &str,
        params: Value,
    ) -> Result<BoxStream<Result<Value>>> {
        self.client
            .stream_raw(
                "services.stream",
                json!({
                    "plugin": plugin,
                    "service": service,
                    "method": method,
                    "params": params,
                }),
                self.call_id,
                self.timeout,
            )
            .await
    }
}

pub struct AuditClient<'a> {
    client: &'a Client,
    call_id: Option<&'a str>,
    timeout: Duration,
}

impl<'a> AuditClient<'a> {
    pub async fn record(
        &self,
        action: &str,
        resource: Option<&str>,
        details: Option<&Value>,
    ) -> Result<()> {
        let mut params = json!({ "action": action });
        if let Some(r) = resource {
            params["resource"] = Value::String(r.to_owned());
        }
        if let Some(d) = details {
            params["details"] = d.clone();
        }
        self.client
            .call_raw("audit.record", params, self.call_id, self.timeout)
            .await?;
        Ok(())
    }
}

/// Sender for streaming RPC chunks.
#[derive(Clone)]
pub struct Chunks<T = Value> {
    tx: mpsc::Sender<Result<T>>,
}

impl<T> Chunks<T> {
    pub async fn send(&self, value: T) -> Result<()> {
        self.tx
            .send(Ok(value))
            .await
            .map_err(|_| RpcError::unavailable("the stream caller disconnected"))
    }
}

type MethodHandler = Arc<dyn Fn(Ctx, Value) -> BoxFuture<'static, Result<Value>> + Send + Sync>;
type StreamHandler =
    Arc<dyn Fn(Ctx, Value, Chunks<Value>) -> BoxFuture<'static, Result<()>> + Send + Sync>;

struct Inner {
    config: Config,
    client: Client,
    methods: RwLock<HashMap<String, MethodHandler>>,
    streams: RwLock<HashMap<String, StreamHandler>>,
}

/// Rookery app-service host: serves inbound `POST /rpc` calls and provides authenticated callbacks.
#[derive(Clone)]
pub struct Rookery {
    inner: Arc<Inner>,
}

impl std::ops::Deref for Rookery {
    type Target = Client;
    fn deref(&self) -> &Self::Target {
        &self.inner.client
    }
}

impl Rookery {
    pub fn from_env() -> Result<Self> {
        Self::new(Config::from_env()?)
    }

    pub fn new(config: Config) -> Result<Self> {
        config.validate()?;
        let client = Client::new(
            config.rookery_url.clone(),
            config.callback_secret.clone(),
            config.plugin_id.clone(),
            config.service_id.clone(),
        );
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                client,
                methods: RwLock::default(),
                streams: RwLock::default(),
            }),
        })
    }

    pub fn client(&self) -> &Client {
        &self.inner.client
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn method<P, R, F, Fut>(&self, name: impl Into<String>, handler: F)
    where
        P: DeserializeOwned + Send + 'static,
        R: Serialize + Send + 'static,
        F: Fn(Ctx, P) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R>> + Send + 'static,
    {
        let h: MethodHandler = Arc::new(move |ctx, raw| match serde_json::from_value::<P>(raw) {
            Ok(p) => {
                let fut = handler(ctx, p);
                Box::pin(async move {
                    let out = fut.await?;
                    serde_json::to_value(out).map_err(|e| RpcError::internal(e.to_string()))
                })
            }
            Err(e) => {
                Box::pin(async move { Err(RpcError::invalid(format!("invalid params: {e}"))) })
            }
        });
        self.inner.methods.write().unwrap().insert(name.into(), h);
    }

    pub fn stream_method<P, F, Fut>(&self, name: impl Into<String>, handler: F)
    where
        P: DeserializeOwned + Send + 'static,
        F: Fn(Ctx, P, Chunks<Value>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let h: StreamHandler =
            Arc::new(
                move |ctx, raw, out| match serde_json::from_value::<P>(raw) {
                    Ok(p) => Box::pin(handler(ctx, p, out)),
                    Err(e) => {
                        Box::pin(
                            async move { Err(RpcError::invalid(format!("invalid params: {e}"))) },
                        )
                    }
                },
            );
        self.inner.streams.write().unwrap().insert(name.into(), h);
    }

    /// Axum router serving `POST /rpc` and `GET /healthz`.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/rpc", post(handle_rpc))
            .route("/healthz", get(|| async { (StatusCode::OK, "ok") }))
            .with_state(self.inner.clone())
    }

    /// Serves inbound RPC requests on an existing listener until `shutdown` resolves.
    pub async fn serve_on(
        &self,
        listener: TcpListener,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        axum::serve(listener, self.router())
            .with_graceful_shutdown(shutdown)
            .await
            .map_err(|e| RpcError::internal(format!("service HTTP server failed: {e}")))
    }

    /// Binds `config.listen` and serves until SIGTERM / Ctrl+C.
    pub async fn serve(&self) -> Result<()> {
        let listener = TcpListener::bind(&self.inner.config.listen)
            .await
            .map_err(|e| {
                RpcError::unavailable(format!("failed to bind {}: {e}", self.inner.config.listen))
            })?;
        tracing::info!(
            listen = %self.inner.config.listen,
            service = %self.inner.config.service_id,
            "rookery app service listening"
        );
        self.serve_on(listener, shutdown_signal()).await
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InboundCtx {
    #[serde(default)]
    call_id: String,
    #[serde(default)]
    user: Option<User>,
    #[serde(default)]
    principal: Option<Principal>,
    #[serde(default)]
    space: Option<String>,
    #[serde(default)]
    deadline: Option<i64>,
}

#[derive(Deserialize)]
struct InboundRequest {
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default = "empty_obj")]
    params: Value,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    ctx: Option<InboundCtx>,
}

fn empty_obj() -> Value {
    json!({})
}

fn rpc_response(status: StatusCode, id: Value, res: Result<Value>) -> Response {
    let body = match res {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    };
    (status, axum::Json(body)).into_response()
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn handle_rpc(
    State(inner): State<Arc<Inner>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim)
    else {
        return rpc_response(
            StatusCode::UNAUTHORIZED,
            Value::Null,
            Err(RpcError::unauthenticated("missing service bearer token")),
        );
    };
    if !secret_eq(token, &inner.config.inbound_secret) {
        return rpc_response(
            StatusCode::UNAUTHORIZED,
            Value::Null,
            Err(RpcError::unauthenticated("invalid service bearer token")),
        );
    }
    let req: InboundRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => {
            return rpc_response(
                StatusCode::BAD_REQUEST,
                Value::Null,
                Err(RpcError::invalid(format!("invalid JSON-RPC request: {e}"))),
            );
        }
    };
    let cancel = CancellationToken::new();
    let ictx = req.ctx.unwrap_or(InboundCtx {
        call_id: String::new(),
        user: None,
        principal: None,
        space: None,
        deadline: None,
    });
    let principal = ictx.principal.unwrap_or_else(|| Principal {
        kind: PrincipalKind::Plugin,
        id: inner.config.service_id.clone(),
    });
    let ctx = Ctx {
        client: inner.client.clone(),
        call_id: ictx.call_id,
        user: ictx.user,
        principal,
        space: ictx.space,
        deadline_ms: ictx.deadline,
        cancel: cancel.clone(),
    };
    if req.stream {
        let handler = inner.streams.read().unwrap().get(&req.method).cloned();
        let Some(handler) = handler else {
            return rpc_response(
                StatusCode::OK,
                req.id,
                Err(RpcError::not_found(format!(
                    "unknown stream method {}",
                    req.method
                ))),
            );
        };
        let (tx, rx) = mpsc::channel::<Result<Value>>(16);
        let chunks = Chunks { tx: tx.clone() };
        tokio::spawn(async move {
            if let Err(err) = handler(ctx, req.params, chunks).await {
                let _ = tx.send(Err(err)).await;
            }
        });
        let id = req.id;
        let guard = Arc::new(CancelOnDrop(cancel));
        let stream = futures_util::stream::unfold(
            (rx, false, id, guard),
            |(mut rx, done, id, guard)| async move {
                if done {
                    return None;
                }
                match rx.recv().await {
                    Some(Ok(chunk)) => {
                        let mut line = serde_json::to_vec(&json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": chunk,
                        }))
                        .unwrap_or_default();
                        line.push(b'\n');
                        Some((
                            Ok::<_, std::io::Error>(axum::body::Bytes::from(line)),
                            (rx, false, id, guard),
                        ))
                    }
                    Some(Err(err)) => {
                        let mut line = serde_json::to_vec(&json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": err,
                        }))
                        .unwrap_or_default();
                        line.push(b'\n');
                        Some((Ok(axum::body::Bytes::from(line)), (rx, true, id, guard)))
                    }
                    None => {
                        let mut line = serde_json::to_vec(&json!({
                            "jsonrpc": "2.0",
                            "id": id,
                        }))
                        .unwrap_or_default();
                        line.push(b'\n');
                        Some((Ok(axum::body::Bytes::from(line)), (rx, true, id, guard)))
                    }
                }
            },
        );
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/x-ndjson")],
            Body::from_stream(stream),
        )
            .into_response();
    }
    let handler = inner.methods.read().unwrap().get(&req.method).cloned();
    let Some(handler) = handler else {
        return rpc_response(
            StatusCode::OK,
            req.id,
            Err(RpcError::not_found(format!(
                "unknown method {}",
                req.method
            ))),
        );
    };
    let _drop_cancel = CancelOnDrop(cancel);
    let res = handler(ctx, req.params).await;
    rpc_response(StatusCode::OK, req.id, res)
}
