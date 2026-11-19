//! What the handlers of hosts share: their errors, the answers about hosts, and a host's own API
//! passed through its link.

use super::{Remote, Serve, auth::Access};
use rookery_api::{Applied, Behind, HostBehind, HostType, Power, PowerState, client, code};
use rookery_core::{rollout, store};
use rookery_proto::RpcError;
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// An answer of a handler; the method gives the value inside.
pub struct Json<T>(pub T);

/// The bytes a handler is given, as JSON params carry them.
pub type Bytes = Vec<u8>;

/// An error on its way out: status, code, message, and what it carries besides.
#[derive(Debug)]
pub struct Fail(u16, String, String, Option<Value>);

impl Fail {
    pub fn new(kind: (u16, &str), message: impl Into<String>) -> Fail {
        Fail(kind.0, kind.1.to_owned(), message.into(), None)
    }

    /// With what the caller is told besides, such as a host's refusal.
    pub fn with(mut self, data: Value) -> Fail {
        self.3 = Some(data);
        self
    }

    /// A host that cannot do something now, and the action that raises its type so it can.
    pub fn refused(refusal: rookery_core::host_type::Refusal) -> Fail {
        Fail::new(code::PRECONDITION, refusal.why.clone())
            .with(json!({"refusal": {"why": refusal.why, "raise": refusal.raise}}))
    }

    pub fn message(&self) -> &str {
        &self.2
    }
}

impl From<Fail> for RpcError {
    /// Each status as the error code of the API; the status and code of before stay in `details`, and a
    /// host that refused travels as `UNAVAILABLE`.
    fn from(Fail(status, kind, message, data): Fail) -> RpcError {
        let error = match status {
            400 | 413 | 429 => RpcError::invalid(message),
            401 | 403 => RpcError::forbidden(message),
            404 => RpcError::not_found(message),
            409 => RpcError::conflict(message),
            412 | 501 | 502 | 503 => RpcError::unavailable(message),
            504 => RpcError::timeout(message),
            _ => RpcError::internal(message),
        };
        let mut details = json!({"code": kind});
        if let Some(Value::Object(data)) = data {
            for (key, value) in data {
                details[key] = value;
            }
        }
        error.with_details(details)
    }
}

impl From<store::Error> for Fail {
    fn from(error: store::Error) -> Fail {
        let kind = match error {
            store::Error::Conflict(_) => code::PRECONDITION,
            store::Error::TooLarge => code::TOO_LARGE,
            store::Error::Taken { .. } => code::CONFLICT,
            store::Error::Given(_) => code::BAD_REQUEST,
            store::Error::Database(_)
            | store::Error::Encrypted { .. }
            | store::Error::Sealing(_) => code::INTERNAL,
        };
        Fail::new(kind, error.to_string())
    }
}

impl From<client::Error> for Fail {
    /// A host's own answer. One that failed inside the host is the host's failure, not the
    /// server's: it goes on as 502, which callers see as UNAVAILABLE, with the host's status.
    fn from(error: client::Error) -> Fail {
        let status = error.status();
        let data = (status >= 500).then(|| json!({"host_status": status}));
        Fail(
            if status == 500 { 502 } else { status },
            error.code().to_owned(),
            error.to_string(),
            data,
        )
    }
}

impl From<RpcError> for Fail {
    fn from(error: RpcError) -> Fail {
        use rookery_proto::ErrorCode::*;
        let kind = match error.error_code() {
            Invalid => code::BAD_REQUEST,
            Forbidden | Unauthenticated => code::FORBIDDEN,
            NotFound => code::NOT_FOUND,
            Conflict => code::CONFLICT,
            Unavailable => code::NOT_SERVED,
            Timeout => code::TIMEOUT,
            _ => code::INTERNAL,
        };
        Fail::new(kind, error.to_string())
    }
}

pub type Answer<T> = std::result::Result<T, Fail>;

pub(crate) fn internal(error: impl std::fmt::Display) -> Fail {
    Fail::new(code::INTERNAL, error.to_string())
}

/// What the core said when it did not do something: its words, and whether
/// it refused before anything began. It crosses threads, as an error of the
/// core does not.
#[derive(Debug)]
pub(crate) struct Said {
    pub refused: bool,
    pub message: String,
}

impl From<Box<dyn std::error::Error>> for Said {
    fn from(error: Box<dyn std::error::Error>) -> Said {
        Said {
            refused: rookery_core::is_refused(error.as_ref()),
            message: error.to_string(),
        }
    }
}

impl From<String> for Said {
    fn from(message: String) -> Said {
        Said {
            refused: false,
            message,
        }
    }
}

impl std::fmt::Display for Said {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// A request the core turned down: refused before anything began is a
/// failed precondition; anything else is a request that does not fit.
pub(crate) fn turned_down(said: impl Into<Said>) -> Fail {
    let said = said.into();
    match said.refused {
        true => Fail::new(code::PRECONDITION, said.message),
        false => Fail::new(code::BAD_REQUEST, said.message),
    }
}

pub fn caller(access: Option<Access>) -> Access {
    access.unwrap_or_else(Access::local)
}

// ── What the answers look like ──────────────────────────────────────────────

pub use rookery_proto::hosts::{HostInfo, HostMachine};

/// What the server knows of a host apart from asking it.
pub(crate) struct Known {
    /// The type of its profile.
    pub host_type: HostType,
    /// A key it logs in with is being rotated.
    pub rotating: bool,
    pub power: Power,
    pub machine: Option<HostMachine>,
}

// ── Hosts ───────────────────────────────────────────────────────────────────

/// What the server would apply now, to compare a host with.
pub(crate) type Compare = Arc<dyn Fn(&Applied) -> Option<Behind> + Send + Sync>;

pub(crate) fn compare(state: &Serve) -> Compare {
    Arc::new(rollout::comparer(&state.paths))
}

/// What the server knows of a host apart from asking it: the type of its
/// profile, whether a key it logs in with is being rotated, and how it is
/// started and stopped.
pub(crate) fn known(state: &Serve, name: &str) -> Known {
    let profile = super::setup::saved_profile(state, name).map(|(_, profile)| profile);
    let machine = super::actions::machine_of(state, name);
    let power = match &profile {
        Ok(profile) => rookery_core::host_type::power(name, profile, machine.as_ref()).power,
        Err(_) => Power::default(),
    };
    let host_type = profile
        .as_ref()
        .map_or(HostType::Workstation, rookery_api::profile::host_type);
    let rotating = profile.is_ok_and(|profile| {
        let place = rookery_core::profiles::Place::own(&state.paths, name);
        let store = rookery_core::store::shared(&state.paths);
        match (place, store) {
            (Ok(place), Ok(store)) => {
                rookery_core::keys::contact::waiting(&store, &place, &profile)
                    .is_ok_and(|keys| !keys.is_empty())
            }
            _ => false,
        }
    });
    Known {
        host_type,
        rotating,
        power,
        machine: machine.map(|machine| HostMachine {
            resource: machine.id,
            provisioner: machine.provisioner,
            external_id: machine.external_id,
            state: machine.state,
        }),
    }
}

pub(crate) async fn info(remote: Arc<Remote>, compare: Compare, known: Known) -> HostInfo {
    let seen = remote.version().await;
    let applied = seen
        .as_ref()
        .ok()
        .and_then(|version| version.applied.clone());
    let observed = rookery_core::host_type::seen(
        seen.as_ref()
            .map_err(|error| error.starts_with(super::NO_ROOKERYD)),
    );
    let reachable = seen.is_ok();
    HostInfo {
        id: remote.id.clone(),
        name: remote.name.clone(),
        address: remote.address.clone(),
        reachable,
        version: seen.as_ref().ok().map(|version| version.version.clone()),
        api: seen.as_ref().ok().map(|version| version.api),
        error: seen.err(),
        behind: applied.as_ref().and_then(|applied| compare(applied)),
        applied: applied.map(|applied| applied.rookery),
        host_type: known.host_type,
        seen: observed,
        rotating: known.rotating,
        power: Power {
            // A host whose controller answers runs, whatever was said before.
            state: match reachable {
                true => PowerState::Running,
                false => known.power.state,
            },
            ..known.power
        },
        machine: known.machine,
    }
}

/// What the server knows of the hosts, from the store, which may block.
pub(crate) async fn knowing(state: &Arc<Serve>, names: Vec<String>) -> Vec<Known> {
    let state = state.clone();
    tokio::task::spawn_blocking(move || names.iter().map(|name| known(&state, name)).collect())
        .await
        .unwrap_or_default()
}

/// The hosts, asked all at once.
pub(crate) async fn all_infos(state: &Arc<Serve>) -> Vec<HostInfo> {
    state.refresh_now(false).await;
    let compare = {
        let state = state.clone();
        tokio::task::spawn_blocking(move || compare(&state))
            .await
            .expect("comparing does not panic")
    };
    let remotes: Vec<_> = state.hosts.read().unwrap().values().cloned().collect();
    let names = remotes.iter().map(|remote| remote.name.clone()).collect();
    let known = knowing(state, names).await;
    futures_util::future::join_all(
        remotes
            .into_iter()
            .zip(known)
            .map(|(remote, known)| info(remote, compare.clone(), known)),
    )
    .await
}

/// The server's hosts that are behind, asked all at once.
pub(crate) async fn behind_hosts(state: &Arc<Serve>) -> Vec<HostBehind> {
    all_infos(state)
        .await
        .into_iter()
        .filter_map(|info| {
            Some(HostBehind {
                name: info.name,
                behind: info.behind?,
            })
        })
        .collect()
}

pub(crate) fn remote(state: &Serve, name: &str) -> Answer<Arc<Remote>> {
    state
        .host(name)
        .ok_or_else(|| Fail::new(code::NOT_FOUND, format!("No host {name}")))
}

/// One host's info.
pub(crate) async fn host_info(state: &Arc<Serve>, name: &str) -> Answer<HostInfo> {
    state.refresh_now(false).await;
    let remote = remote(state, name)?;
    let known = {
        let (state, name) = (state.clone(), name.to_owned());
        tokio::task::spawn_blocking(move || (known(&state, &name), compare(&state)))
            .await
            .map_err(internal)?
    };
    Ok(info(remote, known.1, known.0).await)
}

// ── A host's own API, passed through ────────────────────────────────────────

/// The link of a host whose controller can answer `capability`, or why not.
pub(crate) async fn controller(
    state: &Arc<Serve>,
    name: &str,
    capability: rookery_core::host_type::Capability,
) -> Answer<Arc<Remote>> {
    state.refresh_now(false).await;
    let remote = remote(state, name)?;
    let host_type = {
        let (state, name) = (state.clone(), name.to_owned());
        tokio::task::spawn_blocking(move || known(&state, &name).host_type)
            .await
            .map_err(internal)?
    };
    rookery_core::host_type::check(name, host_type, capability).map_err(Fail::refused)?;
    if remote.link.is_none() {
        let problem = remote.problem.clone().unwrap_or_else(|| "no link".into());
        return Err(Fail::new(code::UNREACHABLE, problem));
    }
    Ok(remote)
}

/// Asks a host's controller, by method and path under `/v1`, with a JSON body: its answer as JSON, or
/// its bytes in base64 under `base64` with their `content_type`.
pub(crate) async fn pass(
    remote: &Remote,
    method: &str,
    target: &str,
    body: Option<&Value>,
) -> Answer<Value> {
    let link = remote
        .link
        .as_ref()
        .ok_or_else(|| Fail::new(code::UNREACHABLE, "no link"))?;
    let (headers, bytes) = match body {
        Some(body) => (
            vec![("content-type", "application/json")],
            serde_json::to_vec(body).map_err(internal)?,
        ),
        None => (vec![], vec![]),
    };
    let answer = link.send(method, target, &headers, &bytes).await?;
    if answer.status >= 400 {
        let said: Value = serde_json::from_slice(&answer.body).unwrap_or(Value::Null);
        let message = said["error"]["message"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| String::from_utf8_lossy(&answer.body).into_owned());
        let kind = said["error"]["code"]
            .as_str()
            .unwrap_or("internal")
            .to_owned();
        // The host's own failure is the host's: 502 for the caller, which says UNAVAILABLE, with
        // the host's status beside it.
        let (status, data) = match answer.status {
            500 => (502, Some(json!({"host_status": 500}))),
            status => (status, None),
        };
        return Err(Fail(status, kind, message, data));
    }
    let content_type = answer
        .headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    if content_type.starts_with("application/json") {
        return Ok(serde_json::from_slice(&answer.body).unwrap_or(Value::Null));
    }
    if answer.body.is_empty() {
        return Ok(json!({}));
    }
    Ok(json!({"content_type": content_type, "base64": rookery_api::b64(&answer.body)}))
}
pub(crate) fn host_key(paths: &rookery_core::paths::Paths, given: &str) -> String {
    rookery_core::profiles::Place::find(paths, store::OWN, given)
        .ok()
        .flatten()
        .map_or_else(|| given.to_owned(), |place| place.key())
}

/// What running an action came to: the job that runs it, the session on the host for one the host
/// runs, or what an action that answers at once said.
#[derive(Debug, Default, Serialize, schemars::JsonSchema)]
pub struct Acted {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<rookery_proto::jobs::JobInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<rookery_api::Session>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}
