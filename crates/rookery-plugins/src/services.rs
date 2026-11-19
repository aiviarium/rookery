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

use std::sync::Arc;
use std::time::Duration;

use rookery_kernel::{Call, Caller, Kernel, Level, Methods, RpcError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Plugins;
use crate::loaded::Loaded;

/// How long one call of an app service may take.
const APP_SERVICE_TIMEOUT: Duration = Duration::from_secs(30);

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

/// `services.setExternal`: an admin places an app service at a URL, with the secret it is called with.
/// Without `secret` the one kept stays; an empty `url` takes the placement away.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AppServiceSetExternal {
    pub plugin: String,
    pub service: String,
    pub url: String,
    #[serde(default)]
    pub secret: Option<String>,
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
fn app_services(plugin: &Loaded) -> Vec<(String, String, bool, Vec<String>)> {
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
        },
        None => AppServiceStatus {
            state: "absent".into(),
            placement: None,
            detail: Some("an admin places it at a URL with services.setExternal".into()),
        },
    }
}

/// Calls a method of an app service placed at a URL, as JSON-RPC 2.0 over `POST <url>/rpc`, with the
/// caller in `ctx`.
async fn call_app_service(
    kernel: &Kernel,
    call: &Call,
    plugin: &str,
    service: &str,
    method: &str,
    params: Value,
) -> Result<Value, RpcError> {
    let Some(url) = placement(kernel, plugin, service).await else {
        return Err(RpcError::unavailable(format!(
            "the app service {service} of {plugin} is placed nowhere: an admin places it with services.setExternal"
        )));
    };
    let secret = secret(kernel, plugin, service).await;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
        "ctx": {"user": call.user(), "principal": call.principal()},
    });
    let endpoint = format!("{}/rpc", url.trim_end_matches('/'));
    let answer = tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(APP_SERVICE_TIMEOUT))
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
    let provider = plugins
        .get(plugin)
        .ok_or_else(|| RpcError::not_found(format!("no plugin {plugin} is installed")))?;
    let Some(m) = manifest(&provider) else {
        return Err(RpcError::unavailable(format!("{plugin} cannot be used")));
    };
    if !strings(m.pointer("/provides/services"))
        .iter()
        .any(|s| s == service)
    {
        return Err(RpcError::not_found(format!(
            "{plugin} provides no service {service}"
        )));
    }
    if let Some((app, ..)) = app_services(&provider)
        .into_iter()
        .find(|(_, _, _, provides)| provides.iter().any(|p| p == service))
    {
        return call_app_service(&call.kernel, call, plugin, &app, method, params).await;
    }
    plugins
        .activate_as(&call.kernel, plugin, call.is_admin())
        .await?;
    plugins
        .host
        .invoke(
            call,
            plugins,
            plugin,
            "service",
            service,
            json!({"method": method, "params": params}),
        )
        .await
}

/// The handler `contributes.rpc` names for a method of a plugin, `(service, method)`, if it names one.
pub fn rpc_handler(plugin: &Loaded, method: &str) -> Option<(String, String, bool)> {
    let entry = manifest(plugin)?
        .pointer("/contributes/rpc")?
        .as_array()?
        .iter()
        .find(|r| r.get("method").and_then(Value::as_str) == Some(method))?;
    let handler = entry.get("handler")?;
    Some((
        handler.get("service")?.as_str()?.to_owned(),
        handler.get("method")?.as_str()?.to_owned(),
        entry
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    ))
}

/// Calls the app service method `contributes.rpc` binds a method to.
pub async fn call_bound(
    call: &Call,
    plugin: &str,
    service: &str,
    method: &str,
    stream: bool,
    params: Value,
) -> Result<Value, RpcError> {
    if stream {
        return Err(RpcError::unavailable(format!(
            "{method} of {plugin} answers with a stream, which this server does not serve yet"
        )));
    }
    call_app_service(&call.kernel, call, plugin, service, method, params).await
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
                call.audit(
                    "service.place",
                    None,
                    json!({"plugin": p.plugin, "service": p.service, "url": p.url}),
                )
                .await;
                Ok(status(Some(&p.url).filter(|u| !u.is_empty()).map(|u| u.as_str())))
            },
        )
        .summary("Places an app service of a plugin at a URL, with the secret the server calls it with.")
        .level(Level::Admin);
}
