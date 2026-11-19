//! `connections.*`: credentials of outside services. A connection has a type, a plain config and a
//! sealed credential. Rookery's own types are here; plugins will declare more.

use std::collections::BTreeMap;

use rookery_kernel::outbound::{self, AddrRange};
use rookery_kernel::{Call, Caller, Empty, Level, Methods, RpcError};
use rookery_proto::IdRef;
use rookery_proto::facilities::{
    ConnectionCreate, ConnectionInfo, ConnectionList, ConnectionResolve, ConnectionResolved,
    ConnectionTested, ConnectionType, ConnectionTypes, ConnectionUpdate, ConnectionsOf,
    Scope as WireScope,
};
use rookery_store::connections::{Change, Connection, Scope};
use serde_json::{Value, json};
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};

use crate::{own_user, parse_id, time};

/// A type of connection Rookery knows.
struct Kind {
    id: &'static str,
    label: &'static str,
    credential: Option<&'static str>,
    /// What `connections.test` asks for, after the config's url.
    probe: &'static str,
}

const KINDS: [Kind; 2] = [
    Kind {
        id: "registry",
        label: "Container registry",
        credential: Some("password"),
        probe: "/v2/",
    },
    Kind {
        id: "http",
        label: "HTTP service",
        credential: Some("token"),
        probe: "",
    },
];

fn kind(id: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|k| k.id == id)
}

struct PluginKind {
    plugin: String,
    wire: ConnectionType,
    validator: jsonschema::Validator,
    credential_required: bool,
    scopes: Vec<String>,
}

/// Connection types declared by the active plugin manifests. A type can hold one sealed credential.
#[derive(Default)]
pub struct PluginKinds(BTreeMap<String, PluginKind>);

impl PluginKinds {
    pub fn from_manifests<'a>(manifests: impl IntoIterator<Item = (&'a str, &'a Value)>) -> Self {
        let mut kinds = BTreeMap::new();
        for (plugin, manifest) in manifests {
            for value in manifest
                .pointer("/contributes/connectionTypes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(entry) = PluginKind::read(plugin, value) {
                    kinds.insert(entry.wire.id.clone(), entry);
                } else {
                    tracing::warn!(plugin, "ignoring an unsupported connection type");
                }
            }
        }
        Self(kinds)
    }
}

impl PluginKind {
    fn read(plugin: &str, value: &Value) -> Option<Self> {
        let id = value["id"].as_str()?;
        // A declaration cannot claim another plugin's credentials.
        if !id
            .strip_prefix(plugin)
            .is_some_and(|suffix| suffix.starts_with('.'))
        {
            return None;
        }
        let mut config = value.get("fields")?.clone();
        if config["type"] != "object" {
            return None;
        }
        let mut credential_required = false;
        // One sealed credential at most: a list of one field's name, or none.
        // Any other JSON type would leave the secret in the plain config.
        let credential = match value.get("secret") {
            None | Some(Value::Null) => None,
            Some(Value::Array(fields)) => match fields.as_slice() {
                [] => None,
                [field] => {
                    let name = field.as_str()?;
                    config
                        .get_mut("properties")?
                        .as_object_mut()?
                        .remove(name)?;
                    if let Some(required) = config.get_mut("required").and_then(Value::as_array_mut)
                    {
                        credential_required =
                            required.iter().any(|field| field.as_str() == Some(name));
                        required.retain(|field| field.as_str() != Some(name));
                    }
                    Some(name.to_owned())
                }
                _ => return None,
            },
            Some(_) => return None,
        };
        config["additionalProperties"] = Value::Bool(false);
        let validator = jsonschema::validator_for(&config).ok()?;
        let scopes = value["scopes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        Some(Self {
            plugin: plugin.to_owned(),
            wire: ConnectionType {
                id: id.to_owned(),
                label: value["title"].as_str()?.to_owned(),
                config,
                credential,
                testable: false,
            },
            validator,
            credential_required,
            scopes,
        })
    }
}

fn check_connection(
    call: &Call,
    id: &str,
    scope: WireScope,
    config: &Value,
    has_credential: bool,
) -> Result<(), RpcError> {
    if let Some(builtin) = kind(id) {
        return check_config(builtin, config);
    }
    let kinds = call.kernel.get_facility::<PluginKinds>();
    let kind = kinds
        .as_ref()
        .and_then(|kinds| kinds.0.get(id))
        .ok_or_else(|| RpcError::invalid(format!("there is no type of connection {id}")))?;
    let scope = match scope {
        WireScope::Global => "global",
        WireScope::User => "user",
    };
    if !kind.scopes.is_empty() && !kind.scopes.iter().any(|s| s == scope) {
        return Err(RpcError::invalid(format!(
            "{id} cannot be used with {scope} scope"
        )));
    }
    if let Some(secret) = &kind.wire.credential
        && config.get(secret).is_some()
    {
        return Err(RpcError::invalid("send the secret field as the credential"));
    }
    if !kind.validator.is_valid(config) {
        return Err(RpcError::invalid(format!(
            "the configuration does not match the schema of {id}"
        )));
    }
    if kind.credential_required && !has_credential {
        return Err(RpcError::invalid(
            "this type of connection needs a credential",
        ));
    }
    if kind.wire.credential.is_none() && has_credential {
        return Err(RpcError::invalid(
            "this type of connection has no credential",
        ));
    }
    Ok(())
}

/// Host halves can see and resolve only the types declared by the plugin on the pipe.
fn require_kind(call: &Call, plugin: &str, id: &str) -> Result<(), RpcError> {
    if kind(id).is_none()
        && call
            .kernel
            .get_facility::<PluginKinds>()
            .is_some_and(|kinds| kinds.0.get(id).is_some_and(|kind| kind.plugin == plugin))
    {
        Ok(())
    } else {
        Err(RpcError::forbidden(
            "a plugin reaches only the connection types it declares",
        ))
    }
}

fn config_schema(kind: &Kind) -> Value {
    let mut properties = json!({ "url": { "type": "string", "format": "uri" } });
    if kind.id == "registry" {
        properties["username"] = json!({ "type": "string" });
    }
    json!({ "type": "object", "required": ["url"], "properties": properties })
}

/// Checks a config against its type: an http or https url, and nothing it does not know.
fn check_config(kind: &Kind, config: &Value) -> Result<(), RpcError> {
    let Some(object) = config.as_object() else {
        return Err(RpcError::invalid("a connection's config is an object"));
    };
    let url = object.get("url").and_then(Value::as_str).unwrap_or("");
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(RpcError::invalid(
            "a connection's config has the url of the service, http:// or https://",
        ));
    }
    let known: &[&str] = if kind.id == "registry" {
        &["url", "username"]
    } else {
        &["url"]
    };
    if let Some(other) = object.keys().find(|k| !known.contains(&k.as_str())) {
        return Err(RpcError::invalid(format!(
            "a {} connection has no {other}",
            kind.id
        )));
    }
    Ok(())
}

fn info(c: Connection) -> ConnectionInfo {
    ConnectionInfo {
        id: c.id.to_string(),
        kind: c.kind,
        name: c.name,
        scope: match c.scope.as_str() {
            "user" => WireScope::User,
            "space" => WireScope::Space,
            _ => WireScope::Global,
        },
        space_id: c.space_id.map(|s| s.to_string()),
        config: c.config,
        has_credential: c.sealed,
        updated_at: time(c.updated_at),
    }
}

/// Whether the caller sees a connection: a global one, their own, or one of a space they hold `need` on.
async fn reaches(
    call: &Call,
    c: &Connection,
    need: rookery_kernel::Grant,
) -> Result<bool, RpcError> {
    let user = call.user().and_then(|u| uuid::Uuid::parse_str(&u.id).ok());
    Ok(match (c.user_id, c.space_id) {
        (Some(owner), _) => Some(owner) == user,
        (None, Some(space)) => {
            crate::access::holds(call, &format!("space://{space}"), need).await?
        }
        (None, None) => true,
    })
}

/// A connection the caller may change: a global one as admin, a space's as a manager of the space, a
/// user's one as that person.
async fn changeable(call: &Call, id: &str) -> Result<Connection, RpcError> {
    let found = call
        .store()
        .connections()
        .get(parse_id("connection", id)?)
        .await?
        .ok_or_else(|| RpcError::not_found("no such connection"))?;
    match (found.user_id, found.space_id) {
        (None, None) => call.require_admin()?,
        _ if reaches(call, &found, rookery_kernel::Grant::Manage).await? => {}
        _ => return Err(RpcError::not_found("no such connection")),
    }
    Ok(found)
}

/// Asks the service whether it answers with the credential.
/// Resolves a name as ureq does, and keeps only the addresses a member may make the server reach: see
/// [`rookery_kernel::outbound`]. Every hop goes through it, redirects too.
#[derive(Debug)]
struct Outside {
    allow: Vec<AddrRange>,
}

impl Resolver for Outside {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let found = DefaultResolver::default().resolve(uri, config, timeout)?;
        let mut kept = self.empty();
        let mut refused = None;
        for addr in found.iter() {
            match outbound::refusal(addr.ip(), &self.allow) {
                None => kept.push(*addr),
                Some(why) => refused = refused.or(Some(why)),
            }
        }
        match refused {
            Some(why) if kept.is_empty() => Err(ureq::Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                why,
            ))),
            _ => Ok(kept),
        }
    }
}

async fn probe(
    url: String,
    credential: Option<String>,
    basic_user: Option<String>,
    allow: Vec<AddrRange>,
) -> ConnectionTested {
    tokio::task::spawn_blocking(move || {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            // A proxy would reach the address for the server, past the check of the resolver.
            .proxy(None)
            .max_redirects(5)
            .build();
        let agent = ureq::Agent::with_parts(
            config,
            ureq::unversioned::transport::DefaultConnector::default(),
            Outside { allow },
        );
        let mut request = agent.get(&url);
        if let Some(secret) = &credential {
            let value = match &basic_user {
                Some(user) => {
                    use base64::Engine;
                    format!(
                        "Basic {}",
                        base64::engine::general_purpose::STANDARD
                            .encode(format!("{user}:{secret}"))
                    )
                }
                None => format!("Bearer {secret}"),
            };
            request = request.header("authorization", &value);
        }
        match request.call() {
            Ok(response) => {
                let status = response.status().as_u16();
                // A registry that asks for a login it was not given still answers.
                let ok = status < 400 || (status == 401 && credential.is_none());
                ConnectionTested {
                    ok,
                    message: format!("{url} answered {status}"),
                }
            }
            Err(e) => ConnectionTested {
                ok: false,
                message: format!("{url}: {e}"),
            },
        }
    })
    .await
    .unwrap_or_else(|e| ConnectionTested {
        ok: false,
        message: e.to_string(),
    })
}

pub fn register(methods: &mut Methods) {
    methods
        .add("connections.types", |call: Call, _: Empty| async move {
            let mut types: Vec<_> = KINDS
                .iter()
                .map(|k| ConnectionType {
                    id: k.id.into(),
                    label: k.label.into(),
                    config: config_schema(k),
                    credential: k.credential.map(Into::into),
                    testable: true,
                })
                .collect();
            if let Some(kinds) = call.kernel.get_facility::<PluginKinds>() {
                types.extend(kinds.0.values().map(|kind| kind.wire.clone()));
            }
            Ok(ConnectionTypes { types })
        })
        .summary("The types of connection and what each holds.")
        .level(Level::Own);
    methods
        .add(
            "connections.list",
            |call: Call, p: ConnectionsOf| async move {
                let plugin = match &call.caller {
                    Caller::Plugin(_) => Some(call.plugin(None)?),
                    _ => None,
                };
                if let (Some(plugin), Some(id)) = (&plugin, &p.kind) {
                    require_kind(&call, plugin, id)?;
                }
                let user = call.user().and_then(|u| uuid::Uuid::parse_str(&u.id).ok());
                let spaces = match p.space.as_deref() {
                    Some(_) => vec![crate::space_held(&call, p.space.as_deref(), rookery_kernel::Grant::View).await?],
                    None => Vec::new(),
                };
                let all = call
                    .store()
                    .connections()
                    .list_in(p.kind.as_deref(), user, &spaces)
                    .await?;
                Ok(ConnectionList {
                    connections: all
                        .into_iter()
                        .filter(|c| {
                            plugin
                                .as_ref()
                                .is_none_or(|plugin| require_kind(&call, plugin, &c.kind).is_ok())
                        })
                        .map(info)
                        .collect(),
                })
            },
        )
        .summary("The global connections, the caller's own and those of a space they name, without credentials.")
        .level(Level::Own);
    methods
        .add("connections.get", |call: Call, p: IdRef| async move {
            let found = call
                .store()
                .connections()
                .get(parse_id("connection", &p.id)?)
                .await?;
            match found {
                Some(c) if reaches(&call, &c, rookery_kernel::Grant::View).await? => {
                    if matches!(call.caller, Caller::Plugin(_)) {
                        require_kind(&call, &call.plugin(None)?, &c.kind)?;
                    }
                    Ok(info(c))
                }
                _ => Err(RpcError::not_found("no such connection")),
            }
        })
        .summary("One connection, without its credential.")
        .level(Level::Own);
    methods
        .add(
            "connections.create",
            |call: Call, p: ConnectionCreate| async move {
                let mut credential = p.credential.as_deref();
                if kind(&p.kind).is_none() {
                    credential = credential.filter(|s| !s.is_empty());
                }
                check_connection(&call, &p.kind, p.scope, &p.config, credential.is_some())?;
                let scope = match p.scope {
                    WireScope::Global => {
                        call.require_admin()?;
                        Scope::Global
                    }
                    WireScope::User => Scope::User(own_user(&call)?),
                    WireScope::Space => Scope::Space(
                        crate::space_held(&call, p.space.as_deref(), rookery_kernel::Grant::Manage).await?,
                    ),
                };
                let made = call
                    .store()
                    .connections()
                    .create(
                        &p.kind,
                        p.name.trim(),
                        scope,
                        &p.config,
                        credential.map(str::as_bytes),
                        &call.audit_name(),
                    )
                    .await?;
                call.audit(
                    "connection.create",
                    Some(&format!("connection://{}", made.id)),
                    json!({ "type": p.kind, "name": made.name }),
                )
                .await;
                Ok(info(made))
            },
        )
        .summary("Makes a connection: a global one as admin, a space's as its manager, or one of the caller's own.")
        .level(Level::Own);
    methods
        .add(
            "connections.update",
            |call: Call, p: ConnectionUpdate| async move {
                let found = changeable(&call, &p.id).await?;
                let credential = p.credential.as_deref().map(|c| {
                    if c.is_empty() {
                        None
                    } else {
                        Some(c.as_bytes())
                    }
                });
                if p.config.is_some() || (credential.is_some() && kind(&found.kind).is_none()) {
                    check_connection(
                        &call,
                        &found.kind,
                        if found.user_id.is_some() {
                            WireScope::User
                        } else {
                            WireScope::Global
                        },
                        p.config.as_ref().unwrap_or(&found.config),
                        credential.map_or(found.sealed, |value| value.is_some()),
                    )?;
                }
                let change = Change {
                    name: p.name.as_deref().map(str::trim),
                    config: p.config.as_ref(),
                    credential,
                };
                let changed = call.store().connections().update(found.id, change).await?;
                call.audit(
                    "connection.update",
                    Some(&format!("connection://{}", found.id)),
                    json!({ "credential": p.credential.is_some() }),
                )
                .await;
                Ok(info(changed))
            },
        )
        .summary("Changes a connection's name, config or credential.")
        .level(Level::Own);
    methods
        .add("connections.delete", |call: Call, p: IdRef| async move {
            let found = changeable(&call, &p.id).await?;
            call.store().connections().delete(found.id).await?;
            call.audit(
                "connection.delete",
                Some(&format!("connection://{}", found.id)),
                json!({}),
            )
            .await;
            Ok(Empty {})
        })
        .summary("Removes a connection.")
        .level(Level::Own);
    methods
        .add("connections.test", |call: Call, p: IdRef| async move {
            let found = changeable(&call, &p.id).await?;
            let kind = kind(&found.kind).ok_or_else(|| RpcError::unavailable("this connection type has no test"))?;
            let credential = call.store().connections().credential(found.id).await?;
            let credential = credential.map(|c| String::from_utf8_lossy(&c).into_owned());
            let url = format!(
                "{}{}",
                found.config["url"]
                    .as_str()
                    .unwrap_or("")
                    .trim_end_matches('/'),
                kind.probe
            );
            let user = found
                .config
                .get("username")
                .and_then(Value::as_str)
                .map(str::to_owned);
            Ok(probe(
                url,
                credential,
                user,
                call.kernel.config.outbound_allow.clone(),
            )
            .await)
        })
        .summary("Asks the service whether it answers with the connection's credential. Internal addresses are refused unless ROOKERY_OUTBOUND_ALLOW allows them.")
        .level(Level::Own);
    methods
        .add(
            "connections.resolve",
            |call: Call, p: ConnectionResolve| async move {
                let plugin = call.plugin(p.plugin.as_deref())?;
                let user = match &call.caller {
                    Caller::User { user, .. } => uuid::Uuid::parse_str(&user.id).ok(),
                    _ => None,
                };
                if let (Caller::Plugin(_), None, Some(id)) = (&call.caller, &p.id, &p.kind) {
                    require_kind(&call, &plugin, id)?;
                }
                let connections = call.store().connections();
                let found = match (&p.id, &p.kind, &p.name) {
                    (Some(id), _, _) => connections
                        .get(parse_id("connection", id)?)
                        .await?
                        // A plugin acts for itself: it uses the global connections and those of spaces.
                        .filter(|c| c.user_id.is_none() || c.user_id == user),
                    (None, Some(kind), Some(name)) => connections.find(kind, name, user).await?,
                    _ => {
                        return Err(RpcError::invalid(
                            "name the connection by id, or by type and name",
                        ));
                    }
                };
                let found = found.ok_or_else(|| RpcError::not_found("no such connection"))?;
                if matches!(call.caller, Caller::Plugin(_)) {
                    require_kind(&call, &plugin, &found.kind)?;
                } else if kind(&found.kind).is_none() {
                    return Err(RpcError::forbidden(
                        "only the declaring plugin can resolve this connection",
                    ));
                }
                let credential = connections.credential(found.id).await?;
                let credential = credential
                    .map(|c| {
                        String::from_utf8(c)
                            .map_err(|_| RpcError::internal("a credential is not text"))
                    })
                    .transpose()?;
                Ok(ConnectionResolved {
                    connection: info(found),
                    credential,
                })
            },
        )
        .summary("A connection with its credential, for the plugin that uses it.")
        .level(Level::Plugin);
}
