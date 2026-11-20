//! What plugins offer each other: extension points and their contributions, and services.
//!
//! - `extensions.list {point}`: the contributions of the valid plugins to a point some plugin declares
//!   (`extensionPoints`), each matching the point's schema. One that does not match is left out, with a
//!   warning in the server's log.
//! - A service is one a plugin names in `provides.services`. Its host half serves it with
//!   `rookery.services.provide(name, methods)`, or an app service of the plugin does: one of its
//!   `services` whose own `provides` names it, which an admin placed at a URL with `services.setExternal`.
//!   `services.call {plugin, service, method, params}` calls it as the caller; an app service is called
//!   with JSON-RPC 2.0 over `POST <url>/rpc`, its secret as a bearer token.
//! - A method of `contributes.rpc` with a `handler` is a method of an app service: `plugins.rpc` calls it.
//! - `services.list` and `services.status` say what app services a plugin declares and where they are.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rookery_kernel::streams::{Initiator, OwnedWriter};
use rookery_proto::User;
use rookery_proto::stream::StreamOpened;
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use rookery_kernel::{Call, Caller, Kernel, Level, Methods, RpcError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Plugins;
use crate::exthost::{Invocation, InvocationPermits, InvocationResult, ServiceMethod, Target};
use crate::loaded::Loaded;

/// How long one call of an app service may take.
const APP_SERVICE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_METHOD_TIMEOUT: Duration = Duration::from_secs(300);
/// The most one line of a streamed app-service answer may hold.
const MAX_LINE: usize = 1024 * 1024;

/// Active app-service invocations, binding server-minted `callId`s to caller identity and deadline.
#[derive(Clone)]
pub(crate) struct AppInvocation {
    pub plugin: String,
    pub service: String,
    pub user: Option<User>,
    pub deadline: Instant,
}

#[derive(Default)]
pub(crate) struct AppCalls(Mutex<HashMap<String, AppInvocation>>);

pub(crate) struct AppCallGuard {
    plugins: Arc<Plugins>,
    id: String,
}

impl Drop for AppCallGuard {
    fn drop(&mut self) {
        self.plugins.app_calls.0.lock().unwrap().remove(&self.id);
    }
}

impl AppCalls {
    pub(crate) fn insert(plugins: &Arc<Plugins>, id: String, inv: AppInvocation) -> AppCallGuard {
        plugins.app_calls.0.lock().unwrap().insert(id.clone(), inv);
        AppCallGuard {
            plugins: plugins.clone(),
            id,
        }
    }

    pub(crate) fn get(&self, id: &str) -> Option<AppInvocation> {
        self.0.lock().unwrap().get(id).cloned()
    }
}

/// SHA-256 hashes of per-service callback credentials, indexed for `POST /services/rpc`.
#[derive(Default)]
pub(crate) struct AppCallbacks(RwLock<HashMap<[u8; 32], (String, String)>>);

fn sha256_bytes(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

fn hex_decode_32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

impl AppCallbacks {
    pub(crate) async fn load(&self, kernel: &Kernel) {
        let Ok(listed) = server_call(
            kernel,
            "storage.list",
            json!({"plugin": ROOKERY, "prefix": "app-service-callback."}),
        )
        .await
        else {
            return;
        };
        let Some(keys) = listed.get("keys").and_then(Value::as_array) else {
            return;
        };
        let mut map = HashMap::new();
        for key in keys.iter().filter_map(Value::as_str) {
            let Ok(entry) = server_call(
                kernel,
                "storage.get",
                json!({"plugin": ROOKERY, "key": key}),
            )
            .await
            else {
                continue;
            };
            let v = &entry["value"];
            if let (Some(hash), Some(plugin), Some(service)) = (
                v["hash"].as_str().and_then(hex_decode_32),
                v["plugin"].as_str(),
                v["service"].as_str(),
            ) {
                map.insert(hash, (plugin.to_owned(), service.to_owned()));
            }
        }
        *self.0.write().unwrap() = map;
    }

    pub(crate) fn set(&self, plugin: &str, service: &str, hash: [u8; 32]) -> Result<(), RpcError> {
        let mut map = self.0.write().unwrap();
        if let Some((other_plugin, other_service)) = map.get(&hash)
            && (other_plugin != plugin || other_service != service)
        {
            return Err(RpcError::conflict(
                "callbackSecret is already used by another app service",
            ));
        }
        map.retain(|_, (p, s)| !(p == plugin && s == service));
        map.insert(hash, (plugin.to_owned(), service.to_owned()));
        Ok(())
    }

    pub(crate) fn remove(&self, plugin: &str, service: &str) {
        self.0
            .write()
            .unwrap()
            .retain(|_, (p, s)| !(p == plugin && s == service));
    }

    pub(crate) fn hash_for(&self, plugin: &str, service: &str) -> Option<[u8; 32]> {
        self.0
            .read()
            .unwrap()
            .iter()
            .find(|(_, (p, s))| p == plugin && s == service)
            .map(|(h, _)| *h)
    }

    pub(crate) fn lookup(&self, secret: &str) -> Option<(String, String)> {
        let hash = sha256_bytes(secret);
        self.0.read().unwrap().get(&hash).cloned()
    }
}

/// The longest a method that applies its own deadline may run.
pub(crate) fn max_timeout() -> Duration {
    MAX_METHOD_TIMEOUT
}

/// `extensions.list`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExtensionQuery {
    pub point: String,
}

/// A contribution to an extension point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExtensionContribution {
    /// The contributing plugin.
    pub plugin: String,
    /// The contribution as declared; it matches the point's schema.
    pub value: Value,
    /// The service of the contributor the contribution names in its `service` field, when the
    /// contributor provides it: the owner of the point calls it with `services.call`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExtensionList {
    pub point: String,
    pub contributions: Vec<ExtensionContribution>,
}

/// `services.call`: a service a plugin provides, by its plugin and name.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ServiceCall {
    pub plugin: String,
    pub service: String,
    pub method: String,
    #[serde(default = "empty")]
    pub params: Value,
}

fn empty() -> Value {
    json!({})
}

/// `services.list`: the app services of one plugin, or of all.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ServicesOf {
    #[serde(default)]
    pub plugin: Option<String>,
}

/// `services.status` and `services.setExternal`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AppServiceRef {
    pub plugin: String,
    pub service: String,
}

/// `services.setExternal`: an admin places an app service at a URL, with the secret it is called with
/// and (optionally) a distinct callback secret the service uses to call Rookery back.
/// Without `secret` the one kept stays; an empty `url` takes the placement away.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AppServiceSetExternal {
    pub plugin: String,
    pub service: String,
    pub url: String,
    #[serde(default)]
    pub secret: Option<String>,
    /// Distinct secret the app service presents on `POST /services/rpc`. Must differ from `secret`.
    /// When omitted on initial placement, Rookery mints one and returns it once in `callbackToken`.
    #[serde(default, rename = "callbackSecret", alias = "callback_secret")]
    pub callback_secret: Option<String>,
}

/// Where an app service is, and whether it can be called.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppServiceStatus {
    /// `ready` when it is placed at a URL; `absent` when it is placed nowhere yet.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Returned once when `services.setExternal` mints a callback credential for the app service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppServiceInfo {
    pub plugin: String,
    pub plugin_name: String,
    pub service: String,
    pub title: String,
    pub optional: bool,
    /// The service of `provides.services` it implements, if one.
    pub provides: Vec<String>,
    pub status: AppServiceStatus,
    /// Where the server sends its calls, never its secret. Admins alone see it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AppServiceList {
    pub services: Vec<AppServiceInfo>,
}

fn plugins(kernel: &Kernel) -> Result<Arc<Plugins>, RpcError> {
    kernel
        .get_facility::<Plugins>()
        .ok_or_else(|| RpcError::unavailable("this server has no plugin folder"))
}

fn manifest(plugin: &Loaded) -> Option<&Value> {
    plugin.manifest.as_ref().filter(|_| plugin.valid())
}

fn strings(value: Option<&Value>) -> Vec<String> {
    if let Some(one) = value.and_then(Value::as_str) {
        return vec![one.to_owned()];
    }
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

pub(crate) fn provides(plugin: &Loaded, service: &str) -> bool {
    manifest(plugin).is_some_and(|m| {
        strings(m.pointer("/provides/services"))
            .iter()
            .any(|name| name == service)
    })
}

fn provider(
    call: &Call,
    plugins: &Plugins,
    plugin: &str,
    service: &str,
) -> Result<Arc<Loaded>, RpcError> {
    let provider = plugins
        .get(plugin)
        .ok_or_else(|| RpcError::not_found(format!("no plugin {plugin} is installed")))?;
    let m = manifest(&provider)
        .ok_or_else(|| RpcError::unavailable(format!("{plugin} cannot be used")))?;
    if !provides(&provider, service) {
        return Err(RpcError::not_found(format!(
            "{plugin} provides no service {service}"
        )));
    }
    let level = m
        .pointer("/provides/serviceLevels")
        .and_then(|levels| levels.get(service))
        .and_then(Value::as_str)
        .unwrap_or("own");
    let allowed = match level {
        "own" => true,
        "plugin" => {
            call.is_admin() || matches!(call.caller, Caller::Plugin(_)) || call.on_behalf.is_some()
        }
        "admin" => call.is_admin(),
        _ => false,
    };
    if !allowed {
        return Err(RpcError::forbidden(format!(
            "calling {plugin}/{service} requires {level} access"
        )));
    }
    Ok(provider)
}

/// The contributions to a point, from the plugins installed now.
pub fn contributions(all: &[Arc<Loaded>], point: &str) -> Vec<ExtensionContribution> {
    let schema = all.iter().filter_map(|p| manifest(p)).find_map(|m| {
        m.get("extensionPoints")?
            .as_array()?
            .iter()
            .find(|p| p.get("id").and_then(Value::as_str) == Some(point))
            .map(|p| p.get("schema").cloned())
    });
    let Some(schema) = schema else {
        return vec![];
    };
    let validator = schema.map(|s| jsonschema::validator_for(&s));
    let mut found = vec![];
    for plugin in all {
        let Some(m) = manifest(plugin) else { continue };
        let provided = strings(m.pointer("/provides/services"));
        for value in m
            .pointer("/contributes/extensions")
            .and_then(|e| e.get(point))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match &validator {
                Some(Ok(validator)) if !validator.is_valid(value) => {
                    tracing::warn!(plugin = %plugin.id, point, "a contribution does not match its point's schema, and is left out");
                    continue;
                }
                Some(Err(e)) => {
                    tracing::warn!(point, error = %e, "the schema of an extension point is not valid");
                    return vec![];
                }
                _ => {}
            }
            let service = value
                .get("service")
                .and_then(Value::as_str)
                .filter(|s| provided.iter().any(|p| p == s))
                .map(str::to_owned);
            found.push(ExtensionContribution {
                plugin: plugin.id.clone(),
                value: value.clone(),
                service,
            });
        }
    }
    found
}

/// The app services a plugin declares, as `(id, title, optional, provides)`.
pub(crate) fn app_services(plugin: &Loaded) -> Vec<(String, String, bool, Vec<String>)> {
    manifest(plugin)
        .and_then(|m| m.get("services"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let id = s.get("id")?.as_str()?.to_owned();
            Some((
                id.clone(),
                s.get("title")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_owned(),
                s.get("optional").and_then(Value::as_bool).unwrap_or(false),
                strings(s.get("provides")),
            ))
        })
        .collect()
}

/// Rookery's own plugin id, which no installed plugin may have, so that what is kept under it is the
/// server's alone.
const ROOKERY: &str = "rookery";

/// Where an app service was placed, and its secret: kept in Rookery's own storage and secrets, not the
/// plugin's, which the plugin reads and writes itself. So a plugin cannot move a service an admin
/// placed, nor read the secret the server calls it with.
fn placement_key(plugin: &str, service: &str) -> String {
    format!("app-service.{plugin}.{service}")
}

fn callback_key(plugin: &str, service: &str) -> String {
    format!("app-service-callback.{plugin}.{service}")
}

async fn server_call(kernel: &Kernel, method: &str, params: Value) -> Result<Value, RpcError> {
    let call = Call::new(kernel.clone(), Caller::System("plugins".into()), None);
    kernel.methods.dispatch(call, method, params).await
}

async fn placement(kernel: &Kernel, plugin: &str, service: &str) -> Option<String> {
    let found = server_call(
        kernel,
        "storage.get",
        json!({"plugin": ROOKERY, "key": placement_key(plugin, service)}),
    )
    .await
    .ok()?;
    found["value"]["url"]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
}

async fn secret(kernel: &Kernel, plugin: &str, service: &str) -> Option<String> {
    let found = server_call(
        kernel,
        "secrets.get",
        json!({"plugin": ROOKERY, "name": placement_key(plugin, service)}),
    )
    .await
    .ok()?;
    found["value"].as_str().map(str::to_owned)
}

fn status(url: Option<&str>) -> AppServiceStatus {
    match url {
        Some(_) => AppServiceStatus {
            state: "ready".into(),
            placement: Some("external".into()),
            detail: None,
            callback_token: None,
        },
        None => AppServiceStatus {
            state: "absent".into(),
            placement: None,
            detail: Some("an admin places it at a URL with services.setExternal".into()),
            callback_token: None,
        },
    }
}

fn app_ctx(call: &Call, call_id: &str) -> Value {
    let left = call.deadline.saturating_duration_since(Instant::now());
    let deadline_ms = chrono::Utc::now().timestamp_millis()
        + i64::try_from(left.as_millis()).unwrap_or(i64::MAX / 2);
    let space = rookery_data::spaces::current(call).map(|s| s.to_string());
    json!({
        "callId": call_id,
        "user": call.user(),
        "principal": call.principal(),
        "space": space,
        "deadline": deadline_ms,
    })
}

/// Calls a method of an app service placed at a URL, as JSON-RPC 2.0 over `POST <url>/rpc`, with the
/// caller and a server-minted `callId` in `ctx`.
pub(crate) async fn call_app_service(
    plugins: &Arc<Plugins>,
    call: &Call,
    plugin: &str,
    service: &str,
    method: &str,
    params: Value,
) -> Result<Value, RpcError> {
    let kernel = &call.kernel;
    let Some(url) = placement(kernel, plugin, service).await else {
        return Err(RpcError::unavailable(format!(
            "the app service {service} of {plugin} is placed nowhere: an admin places it with services.setExternal"
        )));
    };
    let secret = secret(kernel, plugin, service).await;
    let call_id = uuid::Uuid::now_v7().to_string();
    let ctx = app_ctx(call, &call_id);
    let _guard = AppCalls::insert(
        plugins,
        call_id,
        AppInvocation {
            plugin: plugin.to_owned(),
            service: service.to_owned(),
            user: call.user().cloned(),
            deadline: call.deadline,
        },
    );
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
        "ctx": ctx,
    });
    let remaining = call
        .deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_secs(1));
    let timeout = remaining.min(MAX_METHOD_TIMEOUT);
    let endpoint = format!("{}/rpc", url.trim_end_matches('/'));
    let answer = tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .build()
            .into();
        let mut request = agent
            .post(&endpoint)
            .header("content-type", "application/json");
        if let Some(secret) = &secret {
            request = request.header("authorization", &format!("Bearer {secret}"));
        }
        let mut response = request
            .send(body.to_string().as_bytes())
            .map_err(|e| format!("{endpoint}: {e}"))?;
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("{endpoint}: {e}"))?;
        serde_json::from_str::<Value>(&text)
            .map_err(|e| format!("{endpoint} answered no JSON-RPC: {e}"))
    })
    .await
    .map_err(|e| RpcError::internal(e.to_string()))?
    .map_err(RpcError::unavailable)?;
    match answer.get("error") {
        Some(error) => Err(serde_json::from_value::<RpcError>(error.clone())
            .unwrap_or_else(|_| RpcError::internal(error.to_string()))),
        None => Ok(answer.get("result").cloned().unwrap_or(Value::Null)),
    }
}

/// Calls a service a plugin provides: its app service's, or its host half's.
pub async fn call_service(
    call: &Call,
    plugins: &Arc<Plugins>,
    plugin: &str,
    service: &str,
    method: &str,
    params: Value,
) -> Result<Value, RpcError> {
    let provider = provider(call, plugins, plugin, service)?;
    if let Some((app, ..)) = app_services(&provider)
        .into_iter()
        .find(|(_, _, _, provides)| provides.iter().any(|p| p == service))
    {
        return tokio::time::timeout_at(
            call.deadline.into(),
            call_app_service(plugins, call, plugin, &app, method, params),
        )
        .await
        .map_err(|_| RpcError::timeout(format!("{plugin}/{service}.{method} timed out")))?;
    }
    let asked = ServiceCall {
        plugin: plugin.into(),
        service: service.into(),
        method: method.into(),
        params,
    };
    let (call, target, permits) = prepare_host(call, plugins, &provider, &asked, false).await?;
    invoke_host(&call, plugins, &provider, target, asked, None, permits)?
        .result()
        .await
}

/// Reserve before activation can spawn work. The activation keeps its permit if the caller leaves.
pub(crate) async fn host_target(
    call: &Call,
    plugins: &Arc<Plugins>,
    expected: &Arc<Loaded>,
    kind: &str,
    key: &str,
    stream: bool,
) -> Result<(Target, InvocationPermits), RpcError> {
    let plugin = &expected.id;
    let permits = plugins.host.reserve_invocation(plugin, stream)?;
    let active = plugins.host.active_link(plugin);
    if let Some(link) = active {
        link.check_plugin(expected)?;
        return Ok((plugins.host.target(plugin, link, kind, key)?, permits));
    }
    if plugins.host.state(plugin).0 == Some(crate::wire::PluginState::Activating)
        && matches!(&call.caller, Caller::Plugin(id) if id == plugin)
    {
        return Err(RpcError::unavailable(
            "a plugin can call its own handlers after activation",
        ));
    }
    let activation = {
        let (plugins, kernel, expected, admin) = (
            plugins.clone(),
            call.kernel.clone(),
            expected.clone(),
            call.is_admin(),
        );
        tokio::spawn(async move {
            let link = plugins.activate_target(&kernel, expected, admin).await?;
            Ok::<_, RpcError>((link, permits))
        })
    };
    let (link, permits) = tokio::time::timeout_at(call.deadline.into(), activation)
        .await
        .map_err(|_| {
            RpcError::timeout(format!(
                "{plugin} did not activate before the call's deadline"
            ))
        })?
        .map_err(|_| RpcError::unavailable(format!("{plugin} did not activate")))??;
    Ok((plugins.host.target(plugin, link, kind, key)?, permits))
}

async fn prepare_host(
    call: &Call,
    plugins: &Arc<Plugins>,
    provider: &Arc<Loaded>,
    asked: &ServiceCall,
    stream: bool,
) -> Result<(Call, Target, InvocationPermits), RpcError> {
    let ServiceCall {
        plugin,
        service,
        method,
        ..
    } = asked;
    let (target, permits) =
        host_target(call, plugins, provider, "service", service, stream).await?;
    let ServiceMethod {
        stream: declared,
        timeout_secs,
    } =
        *target.registration.methods.get(method).ok_or_else(|| {
            RpcError::not_found(format!("{plugin}/{service} has no method {method}"))
        })?;
    if stream != declared {
        let use_method = if declared {
            "services.stream"
        } else {
            "services.call"
        };
        return Err(RpcError::invalid(format!(
            "use {use_method} for {plugin}/{service}.{method}"
        )));
    }
    let mut call = call.clone();
    if let Some(secs) = timeout_secs {
        call.deadline = Instant::now() + Duration::from_secs(secs);
    }
    if call.deadline <= Instant::now() {
        return Err(RpcError::timeout(format!(
            "{plugin}/{service}.{method} timed out"
        )));
    }
    Ok((call, target, permits))
}

fn invoke_host(
    call: &Call,
    plugins: &Plugins,
    provider: &Arc<Loaded>,
    target: Target,
    asked: ServiceCall,
    output: Option<Invocation>,
    permits: InvocationPermits,
) -> Result<InvocationResult, RpcError> {
    plugins.with_current(provider, || {
        plugins.host.begin(
            call,
            target,
            json!({"method": asked.method, "params": asked.params}),
            output,
            permits,
        )
    })
}

/// A stream has room for one unread chunk. The provider waits for room before yielding another.
pub struct ServiceStream {
    items: mpsc::Receiver<Value>,
    result: InvocationResult,
    deadline: Instant,
    _capacity: Option<Arc<OwnedSemaphorePermit>>,
}

impl ServiceStream {
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(crate) async fn next(&mut self) -> Result<Option<Value>, RpcError> {
        tokio::time::timeout_at(self.deadline.into(), async {
            if let Some(item) = self.items.recv().await {
                return Ok(Some(item));
            }
            self.result.result().await?;
            Ok(None)
        })
        .await
        .map_err(|_| RpcError::timeout("the service stream timed out"))?
    }
}

pub(crate) async fn open_stream(
    call: &Call,
    plugins: &Arc<Plugins>,
    asked: ServiceCall,
    caller_stream: Option<Arc<OwnedSemaphorePermit>>,
) -> Result<ServiceStream, RpcError> {
    let provider = provider(call, plugins, &asked.plugin, &asked.service)?;
    if let Some((app, ..)) = app_services(&provider)
        .into_iter()
        .find(|(_, _, _, names)| names.contains(&asked.service))
    {
        return stream_app_service(
            plugins,
            call,
            &asked.plugin,
            &app,
            &asked.method,
            asked.params,
        )
        .await;
    }
    let (call, target, mut permits) = prepare_host(call, plugins, &provider, &asked, true).await?;
    let capacity = permits.stream.clone();
    permits.caller_stream = caller_stream;
    let deadline = call.deadline;
    let (sender, items) = mpsc::channel(1);
    let result = invoke_host(
        &call,
        plugins,
        &provider,
        target,
        asked,
        Some(Invocation::Stream {
            items: sender,
            cancelled: CancellationToken::new(),
        }),
        permits,
    )?;
    Ok(ServiceStream {
        items,
        result,
        deadline,
        _capacity: capacity,
    })
}

pub(crate) async fn forward_stream(mut source: ServiceStream, writer: OwnedWriter) {
    let deadline = source.deadline;
    let send = async {
        while let Some(item) = source.next().await? {
            let mut line = serde_json::to_vec(&item).expect("a JSON value serializes");
            line.push(b'\n');
            writer.write(&line).await?;
        }
        Ok::<_, RpcError>(())
    };
    let done = tokio::select! {
        result = tokio::time::timeout_at(deadline.into(), send) =>
            result.unwrap_or_else(|_| Err(RpcError::timeout("the service stream timed out"))),
        _ = writer.closed() => return,
    };
    match done {
        Ok(()) => {
            writer.eof();
            writer.close(None);
        }
        Err(e) => writer.close(Some(e.body())),
    }
}

/// A method `contributes.rpc` binds to a service of the plugin.
pub struct Bound {
    pub service: String,
    pub method: String,
    /// The method answers with a stream.
    pub stream: bool,
    /// How long the whole stream may last, from the entry's `timeoutSecs`.
    pub timeout_secs: Option<u64>,
}

/// The handler `contributes.rpc` names for a method of a plugin, if it names one.
pub fn rpc_handler(plugin: &Loaded, method: &str) -> Option<Bound> {
    let entry = manifest(plugin)?
        .pointer("/contributes/rpc")?
        .as_array()?
        .iter()
        .find(|r| r.get("method").and_then(Value::as_str) == Some(method))?;
    let handler = entry.get("handler")?;
    Some(Bound {
        service: handler.get("service")?.as_str()?.to_owned(),
        method: handler.get("method")?.as_str()?.to_owned(),
        stream: entry
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        timeout_secs: entry.get("timeoutSecs").and_then(Value::as_u64),
    })
}

/// Calls a method `contributes.rpc` binds to a service: the host half's, or a placed app service's,
/// as `services.call` routes it.
pub async fn call_bound(
    call: &Call,
    plugins: &Arc<Plugins>,
    plugin: &str,
    bound: &Bound,
    params: Value,
) -> Result<Value, RpcError> {
    if bound.stream {
        return Err(RpcError::invalid(format!(
            "{} of {plugin} answers with a stream: use plugins.rpc.stream",
            bound.method
        )));
    }
    // A handler names an app service it ships, or a service name the plugin provides.
    let loaded = plugins
        .get(plugin)
        .ok_or_else(|| RpcError::not_found(format!("no plugin {plugin} is installed")))?;
    if app_services(&loaded)
        .iter()
        .any(|(id, ..)| id == &bound.service)
    {
        return call_app_service(plugins, call, plugin, &bound.service, &bound.method, params)
            .await;
    }
    call_service(call, plugins, plugin, &bound.service, &bound.method, params).await
}

/// Opens the stream a `contributes.rpc` method binds to a service: a host half's, with the credit and
/// cancellation of `services.stream`, or an app service's, read from one JSON-RPC answer per line.
pub(crate) async fn open_bound_stream(
    call: &Call,
    plugins: &Arc<Plugins>,
    plugin: &str,
    bound: &Bound,
    params: Value,
) -> Result<ServiceStream, RpcError> {
    // The entry's `timeoutSecs` bounds the whole stream; a streamed method defaults to 30 seconds.
    let secs = bound
        .timeout_secs
        .unwrap_or(30)
        .clamp(1, MAX_METHOD_TIMEOUT.as_secs());
    let mut call = call.clone();
    call.deadline = Instant::now() + Duration::from_secs(secs);
    let loaded = plugins
        .get(plugin)
        .ok_or_else(|| RpcError::not_found(format!("no plugin {plugin} is installed")))?;
    // The handler names an app service it ships, or a service name the plugin provides.
    if app_services(&loaded)
        .iter()
        .any(|(id, ..)| id == &bound.service)
    {
        return stream_app_service(
            plugins,
            &call,
            plugin,
            &bound.service,
            &bound.method,
            params,
        )
        .await;
    }
    let provider = provider(&call, plugins, plugin, &bound.service)?;
    if let Some((app, ..)) = app_services(&provider)
        .into_iter()
        .find(|(_, _, _, names)| names.iter().any(|name| name == &bound.service))
    {
        return stream_app_service(plugins, &call, plugin, &app, &bound.method, params).await;
    }
    let asked = ServiceCall {
        plugin: plugin.into(),
        service: bound.service.clone(),
        method: bound.method.clone(),
        params,
    };
    open_stream(&call, plugins, asked, None).await
}

/// A streamed answer of an app service: the response's lines are its items, and one with an error
/// ends it. A body that ends is a stream that ended well.
async fn stream_app_service(
    plugins: &Arc<Plugins>,
    call: &Call,
    plugin: &str,
    service: &str,
    method: &str,
    params: Value,
) -> Result<ServiceStream, RpcError> {
    let kernel = &call.kernel;
    let Some(url) = placement(kernel, plugin, service).await else {
        return Err(RpcError::unavailable(format!(
            "the app service {service} of {plugin} is placed nowhere: an admin places it with services.setExternal"
        )));
    };
    let secret = secret(kernel, plugin, service).await;
    let call_id = uuid::Uuid::now_v7().to_string();
    let ctx = app_ctx(call, &call_id);
    let guard = AppCalls::insert(
        plugins,
        call_id,
        AppInvocation {
            plugin: plugin.to_owned(),
            service: service.to_owned(),
            user: call.user().cloned(),
            deadline: call.deadline,
        },
    );
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
        "stream": true,
        "ctx": ctx,
    });
    let endpoint = format!("{}/rpc", url.trim_end_matches('/'));
    let (sender, items) = mpsc::channel(1);
    let (settle, result) = InvocationResult::pair();
    // Every blocking read ends: a stalled service costs its thread for at most the idle read limit,
    // and the whole stream for at most its deadline.
    let remaining = call
        .deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_secs(1));
    let connect = APP_SERVICE_TIMEOUT.min(remaining);
    let idle = APP_SERVICE_TIMEOUT.min(remaining);
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(remaining))
            .timeout_connect(Some(connect))
            .timeout_send_request(Some(connect))
            .timeout_recv_response(Some(connect))
            .timeout_recv_body(Some(idle))
            .timeout_per_read(Some(idle))
            .build()
            .into();
        let mut request = agent
            .post(&endpoint)
            .header("content-type", "application/json")
            .header("accept", "application/x-ndjson");
        if let Some(secret) = &secret {
            request = request.header("authorization", &format!("Bearer {secret}"));
        }
        let fail = |settle: oneshot::Sender<Result<Value, RpcError>>, error: RpcError| {
            let _ = settle.send(Err(error));
        };
        let mut response = match request.send(body.to_string().as_bytes()) {
            Ok(response) => response,
            Err(e) => return fail(settle, RpcError::unavailable(format!("{endpoint}: {e}"))),
        };
        let status = response.status();
        if !status.is_success() {
            return fail(
                settle,
                RpcError::unavailable(format!("{endpoint} answered {status}")),
            );
        }
        let mut reader = response.body_mut().as_reader();
        let mut pending: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            // The caller went away: stop reading, and let the response close with the request.
            if sender.is_closed() {
                return;
            }
            match std::io::Read::read(&mut reader, &mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    pending.extend_from_slice(&chunk[..n]);
                    while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = pending.drain(..=end).collect();
                        let Ok(message) = serde_json::from_slice::<Value>(&line[..end]) else {
                            continue;
                        };
                        if let Some(error) = message.get("error") {
                            let error = serde_json::from_value::<RpcError>(error.clone())
                                .unwrap_or_else(|_| RpcError::internal(error.to_string()));
                            let _ = settle.send(Err(error));
                            return;
                        }
                        // A message with neither a result nor an error, such as the id alone, ends
                        // the stream: it carries no item.
                        let Some(item) = message.get("result").cloned() else {
                            let _ = settle.send(Ok(Value::Null));
                            return;
                        };
                        if sender.blocking_send(item).is_err() {
                            return;
                        }
                    }
                    if pending.len() > MAX_LINE {
                        return fail(
                            settle,
                            RpcError::unavailable(format!(
                                "{endpoint} sent a line longer than {MAX_LINE} bytes"
                            )),
                        );
                    }
                }
                Err(e) => {
                    return fail(settle, RpcError::unavailable(format!("{endpoint}: {e}")));
                }
            }
        }
        let _ = settle.send(Ok(Value::Null));
    });
    Ok(ServiceStream {
        items,
        result,
        deadline: call.deadline,
        _capacity: None,
    })
}

async fn infos(call: &Call, plugins: &Plugins, only: Option<&str>) -> Vec<AppServiceInfo> {
    let mut found = vec![];
    for plugin in plugins.all() {
        if only.is_some_and(|id| id != plugin.id) {
            continue;
        }
        for (service, title, optional, provides) in app_services(&plugin) {
            let url = placement(&call.kernel, &plugin.id, &service).await;
            found.push(AppServiceInfo {
                plugin: plugin.id.clone(),
                plugin_name: plugin.label.clone(),
                service,
                title,
                optional,
                provides,
                status: status(url.as_deref()),
                url: url.filter(|_| call.is_admin()),
            });
        }
    }
    found
}

/// Methods an app service may call back on `POST /services/rpc` as the person an invocation serves
/// (when `callId` carries a person) or as its own plugin (when plugin-level or serving no person).
const APP_CALLBACK_PERSON: &[&str] = &[
    "data.find",
    "data.scan",
    "data.count",
    "data.aggregate",
    "data.search",
    "data.get",
    "data.tail",
    "data.insert",
    "data.upsert",
    "data.update",
    "data.delete",
    "data.restore",
    "data.purge",
    "data.batch",
    "data.import",
    "data.history",
    "data.refsTo",
    "data.refsFrom",
    "documents.create",
    "documents.get",
    "documents.list",
    "documents.delete",
    "documents.read",
    "documents.convert",
    "documents.edit",
    "documents.history",
    "documents.saveVersion",
    "documents.restore",
    "documents.flush",
    "entities.resolve",
    "entities.resolveMany",
    "entities.search",
    "entities.text",
    "spaces.list",
    "spaces.get",
    "spaces.kinds",
    "users.search",
    "blobs.get",
    "blobs.put",
    "blobs.delete",
    "blobs.url",
    "connections.list",
    "connections.get",
    "access.filter",
    "access.grant",
    "access.grants",
    "access.level",
    "access.levelOf",
    "access.require",
    "access.revoke",
    "extensions.list",
    "services.list",
    "services.status",
    "services.call",
    "services.stream",
    "jobs.enqueue",
    "jobs.get",
    "jobs.cancel",
    "events.publish",
    "topics.publish",
    "notify.send",
    "audit.list",
];

/// Methods an app service may call back on `POST /services/rpc` that ALWAYS stay at the plugin's
/// own level (`Caller::Plugin`), never escalating to a person or admin. When `callId` carries a
/// person, `call.acting_user` is set so `connections.resolve` can see that person's own or space
/// connections of the plugin's own declared type.
const APP_CALLBACK_PLUGIN: &[&str] = &[
    "connections.resolve",
    "entities.register",
    "entities.remove",
    "entities.alias",
    "settings.get",
    "settings.set",
    "settings.reset",
    "storage.get",
    "storage.set",
    "storage.list",
    "storage.delete",
    "secrets.get",
    "secrets.set",
    "secrets.list",
    "secrets.delete",
    "secrets.rename",
    "leases.acquire",
    "leases.renew",
    "leases.release",
    "audit.record",
    "sql.query",
    "sql.execute",
    "sql.begin",
    "sql.txQuery",
    "sql.txExecute",
    "sql.txPublish",
    "sql.commit",
    "sql.rollback",
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CallbackRequest {
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default = "empty")]
    params: Value,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    stream: bool,
}

fn rpc_json(status: StatusCode, id: Value, res: Result<Value, RpcError>) -> Response {
    let body = match res {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    };
    (status, axum::Json(body)).into_response()
}

/// `POST /services/rpc`: authenticated callback endpoint for placed app services.
///
/// Authenticates strictly against the per-service callback credential (never the inbound secret or
/// a user token). When `callId` is supplied, binds to that active invocation of the authenticated
/// service: person-level methods run as the person it serves (with `on_behalf`), plugin-level
/// methods stay `Caller::Plugin` with `acting_user` set. Without `callId`, runs at the plugin's
/// own level.
pub async fn callback(
    State(kernel): State<Kernel>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let Some(plugins) = kernel.get_facility::<Plugins>() else {
        return rpc_json(
            StatusCode::SERVICE_UNAVAILABLE,
            Value::Null,
            Err(RpcError::unavailable("this server has no plugin folder")),
        );
    };
    if plugins.ready_soon().await.is_err() {
        return rpc_json(
            StatusCode::SERVICE_UNAVAILABLE,
            Value::Null,
            Err(RpcError::unavailable("the server is reading its plugins")),
        );
    }
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return rpc_json(
            StatusCode::UNAUTHORIZED,
            Value::Null,
            Err(RpcError::unauthenticated(
                "missing service callback bearer token",
            )),
        );
    };
    let Some((plugin, service)) = plugins.callbacks.lookup(token) else {
        return rpc_json(
            StatusCode::UNAUTHORIZED,
            Value::Null,
            Err(RpcError::unauthenticated(
                "invalid service callback credential",
            )),
        );
    };
    let req: CallbackRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => {
            return rpc_json(
                StatusCode::BAD_REQUEST,
                Value::Null,
                Err(RpcError::invalid(format!("invalid JSON-RPC callback: {e}"))),
            );
        }
    };
    let method = req.method.as_str();
    let as_person = APP_CALLBACK_PERSON.contains(&method);
    let as_plugin = APP_CALLBACK_PLUGIN.contains(&method);
    if !as_person && !as_plugin {
        return rpc_json(
            StatusCode::OK,
            req.id,
            Err(RpcError::forbidden(format!(
                "{method} is not allowed on /services/rpc"
            ))),
        );
    }
    let mut call = Call::new(kernel.clone(), Caller::Plugin(plugin.clone()), None);
    if let Some(call_id) = req.call_id.as_deref() {
        let Some(inv) = plugins.app_calls.get(call_id) else {
            return rpc_json(
                StatusCode::OK,
                req.id,
                Err(RpcError::not_found(
                    "the invocation for this callId does not exist or has ended",
                )),
            );
        };
        if inv.plugin != plugin || inv.service != service {
            return rpc_json(
                StatusCode::OK,
                req.id,
                Err(RpcError::forbidden(
                    "this callId does not belong to this app service",
                )),
            );
        }
        call.deadline = inv.deadline;
        if let Some(user) = inv.user {
            if as_person && kernel.methods.level(method) != Some(Level::Plugin) {
                call.on_behalf = Some(plugin.clone());
                call.caller = Caller::User {
                    user,
                    session: None,
                    token: None,
                };
            } else {
                call.acting_user = Some(user);
            }
        }
    }
    if req.stream {
        if method != "services.stream" {
            return rpc_json(
                StatusCode::OK,
                req.id,
                Err(RpcError::invalid(format!(
                    "{method} does not support streaming on /services/rpc"
                ))),
            );
        }
        let asked: ServiceCall = match serde_json::from_value(req.params) {
            Ok(asked) => asked,
            Err(e) => {
                return rpc_json(
                    StatusCode::OK,
                    req.id,
                    Err(RpcError::invalid(format!("params: {e}"))),
                );
            }
        };
        let source = match open_stream(&call, &plugins, asked, None).await {
            Ok(source) => source,
            Err(e) => return rpc_json(StatusCode::OK, req.id, Err(e)),
        };
        let id = req.id;
        let stream = futures_util::stream::unfold(
            (source, false, id),
            |(mut source, done, id)| async move {
                if done {
                    return None;
                }
                match source.next().await {
                    Ok(Some(item)) => {
                        let mut line = serde_json::to_vec(&json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": item,
                        }))
                        .unwrap_or_default();
                        line.push(b'\n');
                        Some((
                            Ok::<_, std::io::Error>(axum::body::Bytes::from(line)),
                            (source, false, id),
                        ))
                    }
                    Ok(None) => {
                        let mut line = serde_json::to_vec(&json!({
                            "jsonrpc": "2.0",
                            "id": id,
                        }))
                        .unwrap_or_default();
                        line.push(b'\n');
                        Some((Ok(axum::body::Bytes::from(line)), (source, true, id)))
                    }
                    Err(err) => {
                        let mut line = serde_json::to_vec(&json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": err,
                        }))
                        .unwrap_or_default();
                        line.push(b'\n');
                        Some((Ok(axum::body::Bytes::from(line)), (source, true, id)))
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
    let result = kernel.methods.dispatch(call, method, req.params).await;
    rpc_json(StatusCode::OK, req.id, result)
}

/// Adds `extensions.list`, `services.call`, `services.list`, `services.status` and `services.setExternal`.
pub fn register(methods: &mut Methods) {
    methods
        .add("extensions.list", |call: Call, p: ExtensionQuery| async move {
            let plugins = plugins(&call.kernel)?;
            Ok(ExtensionList {
                contributions: contributions(&plugins.all(), &p.point),
                point: p.point,
            })
        })
        .summary("The contributions of the installed plugins to an extension point, each matching its schema.")
        .level(Level::Own);
    methods
        .add("services.call", |call: Call, p: ServiceCall| async move {
            let plugins = plugins(&call.kernel)?;
            call_service(&call, &plugins, &p.plugin, &p.service, &p.method, p.params).await
        })
        .summary("Calls a method of a service a plugin provides, as the caller: its host half's, or its app service's.")
        .level(Level::Own)
        .max_timeout(MAX_METHOD_TIMEOUT);
    methods
        .add("services.stream", |call: Call, p: ServiceCall| async move {
            let plugins = plugins(&call.kernel)?;
            let conn = call.conn()?;
            if matches!(call.caller, Caller::Plugin(_)) {
                return Err(RpcError::forbidden(
                    "host halves stream services through the extension host",
                ));
            }
            let source = open_stream(&call, &plugins, p, None).await?;
            let stream = conn.streams().open(Initiator::Peer)?;
            let id = stream.id();
            let (_reader, writer) = stream.split();
            tokio::spawn(forward_stream(source, writer));
            Ok(StreamOpened {
                stream: id,
                offset: None,
            })
        })
        .summary("Streams a service method (host half or app service) as one JSON value per line.")
        .stream()
        .level(Level::Own);
    methods
        .add("services.list", |call: Call, p: ServicesOf| async move {
            let plugins = plugins(&call.kernel)?;
            Ok(AppServiceList {
                services: infos(&call, &plugins, p.plugin.as_deref()).await,
            })
        })
        .summary("The app services the plugins declare, and where each is placed.")
        .level(Level::Own);
    methods
        .add(
            "services.status",
            |call: Call, p: AppServiceRef| async move {
                let plugins = plugins(&call.kernel)?;
                let info = infos(&call, &plugins, Some(&p.plugin))
                    .await
                    .into_iter()
                    .find(|i| i.service == p.service)
                    .ok_or_else(|| {
                        RpcError::not_found(format!(
                            "{} has no app service {}",
                            p.plugin, p.service
                        ))
                    })?;
                Ok(info.status)
            },
        )
        .summary("Where one app service is placed, and whether it can be called.")
        .level(Level::Own);
    methods
        .add(
            "services.setExternal",
            |call: Call, p: AppServiceSetExternal| async move {
                let plugins = plugins(&call.kernel)?;
                let plugin = plugins
                    .get(&p.plugin)
                    .ok_or_else(|| RpcError::not_found(format!("no plugin {}", p.plugin)))?;
                if !app_services(&plugin).iter().any(|(id, ..)| *id == p.service) {
                    return Err(RpcError::not_found(format!(
                        "{} declares no app service {}",
                        p.plugin, p.service
                    )));
                }
                if !p.url.is_empty() && !(p.url.starts_with("http://") || p.url.starts_with("https://")) {
                    return Err(RpcError::invalid("an app service's url starts with http:// or https://"));
                }
                let existing_inbound = secret(&call.kernel, &p.plugin, &p.service).await;
                let effective_inbound = p.secret.as_deref().or(existing_inbound.as_deref());
                let mut minted_callback: Option<String> = None;
                let new_callback_hash: Option<[u8; 32]> = if p.url.is_empty() {
                    None
                } else if let Some(cb) = &p.callback_secret {
                    if cb.trim().is_empty() {
                        return Err(RpcError::invalid("callbackSecret must not be empty"));
                    }
                    if effective_inbound == Some(cb.as_str()) {
                        return Err(RpcError::invalid(
                            "callbackSecret must differ from the inbound secret",
                        ));
                    }
                    Some(sha256_bytes(cb))
                } else if let Some(existing_cb) = plugins.callbacks.hash_for(&p.plugin, &p.service) {
                    if let Some(inbound) = &p.secret
                        && sha256_bytes(inbound) == existing_cb
                    {
                        return Err(RpcError::invalid(
                            "inbound secret must differ from callbackSecret",
                        ));
                    }
                    None
                } else {
                    let minted = rookery_store::util::random_token("rksvc_");
                    let hash = sha256_bytes(&minted);
                    minted_callback = Some(minted);
                    Some(hash)
                };
                if let Some(hash) = new_callback_hash {
                    plugins.callbacks.set(&p.plugin, &p.service, hash)?;
                }
                server_call(
                    &call.kernel,
                    "storage.set",
                    json!({"plugin": ROOKERY, "key": placement_key(&p.plugin, &p.service), "value": {"url": p.url}}),
                )
                .await?;
                if let Some(secret) = &p.secret {
                    server_call(
                        &call.kernel,
                        "secrets.set",
                        json!({"plugin": ROOKERY, "name": placement_key(&p.plugin, &p.service), "value": secret}),
                    )
                    .await?;
                }
                if p.url.is_empty() {
                    plugins.callbacks.remove(&p.plugin, &p.service);
                    let _ = server_call(
                        &call.kernel,
                        "storage.delete",
                        json!({"plugin": ROOKERY, "key": callback_key(&p.plugin, &p.service)}),
                    )
                    .await;
                } else if let Some(hash) = new_callback_hash {
                    server_call(
                        &call.kernel,
                        "storage.set",
                        json!({
                            "plugin": ROOKERY,
                            "key": callback_key(&p.plugin, &p.service),
                            "value": {
                                "hash": hex_encode(&hash),
                                "plugin": p.plugin,
                                "service": p.service,
                            }
                        }),
                    )
                    .await?;
                }
                call.audit(
                    "service.place",
                    None,
                    json!({"plugin": p.plugin, "service": p.service, "url": p.url}),
                )
                .await;
                let mut out = status(Some(&p.url).filter(|u| !u.is_empty()).map(|u| u.as_str()));
                out.callback_token = minted_callback;
                Ok(out)
            },
        )
        .summary("Places an app service of a plugin at a URL, with its inbound secret and distinct callback secret.")
        .level(Level::Admin);
}
