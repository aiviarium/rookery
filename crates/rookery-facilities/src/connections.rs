//! `connections.*`: credentials of outside services. A connection has a type, a plain config and a
//! sealed credential. Rookery's own types are here; plugins will declare more.

use rookery_kernel::{Call, Caller, Empty, Level, Methods, RpcError};
use rookery_proto::IdRef;
use rookery_proto::facilities::{
    ConnectionCreate, ConnectionInfo, ConnectionList, ConnectionResolve, ConnectionResolved,
    ConnectionTested, ConnectionType, ConnectionTypes, ConnectionUpdate, ConnectionsOf,
    Scope as WireScope,
};
use rookery_store::connections::{Change, Connection};
use rookery_store::secrets::Scope;
use serde_json::{Value, json};

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

fn kind(id: &str) -> Result<&'static Kind, RpcError> {
    KINDS
        .iter()
        .find(|k| k.id == id)
        .ok_or_else(|| RpcError::invalid(format!("there is no type of connection {id}")))
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
        scope: if c.scope == "user" {
            WireScope::User
        } else {
            WireScope::Global
        },
        config: c.config,
        has_credential: c.sealed,
        updated_at: time(c.updated_at),
    }
}

/// A connection the caller may change: a global one as admin, a user's one as that person.
async fn changeable(call: &Call, id: &str) -> Result<Connection, RpcError> {
    let found = call
        .store()
        .connections()
        .get(parse_id("connection", id)?)
        .await?
        .ok_or_else(|| RpcError::not_found("no such connection"))?;
    match found.user_id {
        None => call.require_admin()?,
        Some(owner)
            if Some(owner) == call.user().and_then(|u| uuid::Uuid::parse_str(&u.id).ok()) => {}
        Some(_) => return Err(RpcError::not_found("no such connection")),
    }
    Ok(found)
}

/// Asks the service whether it answers with the credential.
async fn probe(
    url: String,
    credential: Option<String>,
    basic_user: Option<String>,
) -> ConnectionTested {
    tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .into();
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
        .add("connections.types", |_: Call, _: Empty| async move {
            let types = KINDS
                .iter()
                .map(|k| ConnectionType {
                    id: k.id.into(),
                    label: k.label.into(),
                    config: config_schema(k),
                    credential: k.credential.map(Into::into),
                    testable: true,
                })
                .collect();
            Ok(ConnectionTypes { types })
        })
        .summary("The types of connection and what each holds.")
        .level(Level::Own);
    methods
        .add(
            "connections.list",
            |call: Call, p: ConnectionsOf| async move {
                let user = call.user().and_then(|u| uuid::Uuid::parse_str(&u.id).ok());
                let all = call
                    .store()
                    .connections()
                    .list(p.kind.as_deref(), user)
                    .await?;
                Ok(ConnectionList {
                    connections: all.into_iter().map(info).collect(),
                })
            },
        )
        .summary("The global connections and the caller's own, without credentials.")
        .level(Level::Own);
    methods
        .add("connections.get", |call: Call, p: IdRef| async move {
            let found = call
                .store()
                .connections()
                .get(parse_id("connection", &p.id)?)
                .await?;
            let user = call.user().and_then(|u| uuid::Uuid::parse_str(&u.id).ok());
            match found {
                Some(c) if c.user_id.is_none() || c.user_id == user => Ok(info(c)),
                _ => Err(RpcError::not_found("no such connection")),
            }
        })
        .summary("One connection, without its credential.")
        .level(Level::Own);
    methods
        .add(
            "connections.create",
            |call: Call, p: ConnectionCreate| async move {
                let kind = kind(&p.kind)?;
                check_config(kind, &p.config)?;
                let scope = match p.scope {
                    WireScope::Global => {
                        call.require_admin()?;
                        Scope::Global
                    }
                    WireScope::User => Scope::User(own_user(&call)?),
                };
                let made = call
                    .store()
                    .connections()
                    .create(
                        kind.id,
                        p.name.trim(),
                        scope,
                        &p.config,
                        p.credential.as_deref().map(str::as_bytes),
                        &call.audit_name(),
                    )
                    .await?;
                call.audit(
                    "connection.create",
                    Some(&format!("connection://{}", made.id)),
                    json!({ "type": kind.id, "name": made.name }),
                )
                .await;
                Ok(info(made))
            },
        )
        .summary("Makes a connection: a global one as admin, or one of the caller's own.")
        .level(Level::Own);
    methods
        .add(
            "connections.update",
            |call: Call, p: ConnectionUpdate| async move {
                let found = changeable(&call, &p.id).await?;
                if let Some(config) = &p.config {
                    check_config(kind(&found.kind)?, config)?;
                }
                let credential = p.credential.as_deref().map(|c| {
                    if c.is_empty() {
                        None
                    } else {
                        Some(c.as_bytes())
                    }
                });
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
            let kind = kind(&found.kind)?;
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
            Ok(probe(url, credential, user).await)
        })
        .summary("Asks the service whether it answers with the connection's credential.")
        .level(Level::Own);
    methods
        .add(
            "connections.resolve",
            |call: Call, p: ConnectionResolve| async move {
                call.plugin(p.plugin.as_deref())?;
                let user = match &call.caller {
                    Caller::User { user, .. } => uuid::Uuid::parse_str(&user.id).ok(),
                    _ => None,
                };
                let connections = call.store().connections();
                let found = match (&p.id, &p.kind, &p.name) {
                    (Some(id), _, _) => connections
                        .get(parse_id("connection", id)?)
                        .await?
                        .filter(|c| c.user_id.is_none() || c.user_id == user),
                    (None, Some(kind), Some(name)) => connections.find(kind, name, user).await?,
                    _ => {
                        return Err(RpcError::invalid(
                            "name the connection by id, or by type and name",
                        ));
                    }
                };
                let found = found.ok_or_else(|| RpcError::not_found("no such connection"))?;
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
