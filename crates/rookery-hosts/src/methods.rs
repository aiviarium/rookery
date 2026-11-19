//! The methods of hosts and of the rest of Rookery's own: `hosts.*`, `keys.*`, `provisioners.*`,
//! `resources.*`, `installation.*`, `setup.*` and `plugins.*`.
//!
//! A host is named by `host`, as its name or as `env://NAME`. Levels on a host: `view` sees it and
//! its state, `use` reaches its plugins' services, `manage` changes it, and the org role admin creates
//! hosts. An action says the role it needs: view for one that only looks, manage for one that operates,
//! admin for one that sets the host up.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use rookery_api::{Act, PluginRegistry, PluginSpec, PluginUpdateSpec};
use rookery_core::host_type::Capability;
use rookery_kernel::streams::Initiator;
use rookery_kernel::{Call, Empty, Grant, Level, Methods, RpcError};
use rookery_proto::hosts::*;
use rookery_proto::stream::StreamOpened;
use serde::Serialize;
use serde_json::{Value, json};

use crate::runner::CALLER;
use crate::serve::api::{self, Answer, Json};
use crate::serve::{Carry, Serve, actions, auth, keys, provisioners, resources, setup};

fn host(grant: Grant) -> Level {
    Level::Resource {
        param: "host".into(),
        grant,
        scheme: Some("env".into()),
    }
}

fn serve(call: &Call) -> Result<Arc<Serve>, RpcError> {
    crate::hosts(&call.kernel).serve()
}

/// Runs a handler as the caller, for the jobs it starts, and gives its answer.
async fn answer<T>(
    call: &Call,
    work: impl Future<Output = Answer<Json<T>>>,
) -> Result<T, RpcError> {
    CALLER
        .scope(call.audit_name(), work)
        .await
        .map(|Json(value)| value)
        .map_err(RpcError::from)
}

async fn access(call: &Call, name: Option<&str>) -> Result<Option<auth::Access>, RpcError> {
    auth::of(call, name).await
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// A host's name from `host`, which may be `env://NAME`.
fn name(host: &str) -> String {
    crate::name_of(host).to_owned()
}

fn acted_of(acted: api::Acted) -> Acted {
    Acted {
        job: acted.job.as_ref().map(|job| job.id.clone()),
        state: acted.job.as_ref().map(|job| job.state),
        info: acted.job,
        session: acted.session,
        result: acted.result,
    }
}

async fn acted(
    call: &Call,
    work: impl Future<Output = Answer<Json<api::Acted>>>,
) -> Result<Acted, RpcError> {
    answer(call, work).await.map(acted_of)
}

fn body(value: &impl Serialize) -> Result<Vec<u8>, RpcError> {
    serde_json::to_vec(value).map_err(|e| RpcError::internal(e.to_string()))
}

fn query_of(query: &BTreeMap<String, String>) -> String {
    if query.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = query
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                rookery_api::client::encode(k),
                rookery_api::client::encode(v)
            )
        })
        .collect();
    format!("?{}", parts.join("&"))
}

async fn pass(
    call: &Call,
    name: &str,
    capability: Capability,
    method: &str,
    target: String,
    body: Option<Value>,
) -> Result<Passed, RpcError> {
    let state = serve(call)?;
    let remote = api::controller(&state, name, capability).await?;
    Ok(Passed(
        api::pass(&remote, method, &target, body.as_ref()).await?,
    ))
}

/// A stream of a host's controller, passed through to the caller on a stream of its connection: the
/// host's text frames as lines, its binary frames as they are, and what the caller sends as binary
/// frames to the host.
async fn pass_stream(
    call: &Call,
    name: &str,
    capability: Capability,
    target: String,
    carry: crate::serve::Carry,
) -> Result<StreamOpened, RpcError> {
    let state = serve(call)?;
    let remote = api::controller(&state, name, capability).await?;
    let link = remote
        .link
        .as_ref()
        .ok_or_else(|| RpcError::unavailable("no link"))?;
    let upstream = link.stream(&target).await.map_err(api::Fail::from)?;
    let stream = call.conn()?.streams().open(Initiator::Peer)?;
    let id = stream.id();
    tokio::spawn(crate::serve::pipe(upstream, stream, carry));
    Ok(StreamOpened {
        stream: id,
        offset: None,
    })
}

// ── Registration ────────────────────────────────────────────────────────────

pub fn register(methods: &mut Methods) {
    register_hosts(methods);
    register_controller(methods);
    register_keys(methods);
    register_env(methods);
    register_messages(methods);
    register_provisioning(methods);
    register_setup(methods);
    register_installation(methods);
    register_machine(methods);
}

/// Refused unless the call comes over the server's own socket.
fn on_this_machine(call: &Call, what: &str) -> Result<(), RpcError> {
    match call.caller {
        rookery_kernel::Caller::Local => Ok(()),
        _ => Err(RpcError::forbidden(format!(
            "{what} names files of the server's machine, and is for its socket alone"
        ))),
    }
}

/// Runs `work` with the server's folders, off the runtime.
async fn with_paths<T: Send + 'static>(
    call: &Call,
    work: impl FnOnce(&rookery_core::paths::Paths) -> Result<T, String> + Send + 'static,
) -> Result<T, RpcError> {
    let paths = serve(call)?.paths.clone();
    tokio::task::spawn_blocking(move || work(&paths))
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?
        .map_err(RpcError::invalid)
}

/// What the server's own machine asks of it for its programs: the inventory and SSH logins of the
/// hosts, and the key an apply joins a tailnet with. Their files are the server's, so only its
/// socket gets them, and no child of the server needs the database.
fn register_machine(methods: &mut Methods) {
    methods
        .add("hosts.inventory", |call: Call, p: HostNames| async move {
            on_this_machine(&call, "hosts.inventory")?;
            let inventory = with_paths(&call, move |paths| {
                use rookery_core::host::Host;
                let hosts: Vec<Host> = match p.hosts.as_slice() {
                    [] => Host::all(paths)
                        .into_iter()
                        .filter(|host| !host.address().is_empty())
                        .collect(),
                    names => names
                        .iter()
                        .map(|name| Host::load(paths, crate::name_of(name)).map_err(|e| e.to_string()))
                        .collect::<Result<_, _>>()?,
                };
                let mut all = serde_json::Map::new();
                for host in hosts {
                    let inventory = host
                        .inventory(paths)
                        .map_err(|error| format!("{}: {error}", host.name))?;
                    all.insert(
                        host.name.clone(),
                        inventory["all"]["hosts"]["rookery_target"].clone(),
                    );
                }
                Ok(json!({"all": {"hosts": all}}))
            })
            .await?;
            Ok(Inventory { inventory })
        })
        .summary("The hosts as an Ansible inventory, with the server's key as a file of its machine. For the server's socket alone.")
        .level(Level::Admin);
    methods
        .add("hosts.ssh", |call: Call, p: HostSsh| async move {
            on_this_machine(&call, "hosts.ssh")?;
            with_paths(&call, move |paths| {
                let host = rookery_core::host::Host::load(paths, crate::name_of(&p.host))
                    .map_err(|e| e.to_string())?;
                if !host.access().reachable() {
                    return Err(format!("{} has no SSH address yet", host.name));
                }
                let command = host.ssh(paths).map_err(|e| e.to_string())?;
                let mut args: Vec<String> = command
                    .get_args()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect();
                args.push(host.target());
                if p.agent {
                    // Root login and the agent's own key are not a given, so it goes through the
                    // SSH user and sudo; agent-env adds the display and tools.
                    let agent = host.agent();
                    if agent.starts_with('-')
                        || !agent
                            .chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
                    {
                        return Err("the agent's account name is not a valid one".into());
                    }
                    args.push("-t".into());
                    args.push(format!(
                        "sudo -iu {agent} sh -c 'command -v agent-env >/dev/null && exec agent-env; exec bash -l'"
                    ));
                }
                let env = command
                    .get_envs()
                    .filter_map(|(key, value)| {
                        Some((key.to_string_lossy().into_owned(), value?.to_string_lossy().into_owned()))
                    })
                    .collect();
                Ok(SshCommand { args, env })
            })
            .await
        })
        .summary("The plain ssh command that logs in to a host with the server's key, for the server's socket alone.")
        .level(Level::Admin);
    methods
        .add("hosts.askpass", |call: Call, p: AskpassAsked| async move {
            on_this_machine(&call, "hosts.askpass")?;
            if !p.source.starts_with("secret:") {
                return Err(RpcError::invalid("the server answers for a password kept as secret:NAME"));
            }
            let answer = with_paths(&call, move |paths| {
                rookery_core::keys::askpass::answer(&p.prompt, &p.source, Some(paths))
                    .map_err(|e| e.to_string())
            })
            .await?;
            Ok(AskpassAnswer { answer })
        })
        .summary("The password ssh asks rookery's askpass helper for, from the secret the helper names. For the server's socket alone.")
        .level(Level::Admin);
    methods
        .add("hosts.tailnetKey", |call: Call, p: HostRef| async move {
            on_this_machine(&call, "hosts.tailnetKey")?;
            let key = with_paths(&call, move |paths| {
                rookery_core::tailnet::key_for(paths, crate::name_of(&p.host)).map_err(|e| e.to_string())
            })
            .await?;
            Ok(TailnetKey { key })
        })
        .summary("A one-use enrollment key for a host's tailnet, which an apply on the server's machine asks for.")
        .level(Level::Admin);
}

fn register_hosts(methods: &mut Methods) {
    methods
        .add("hosts.list", |call: Call, _: Empty| async move {
            let state = serve(&call)?;
            let mut hosts = vec![];
            for info in api::all_infos(&state).await {
                let seen = call.is_admin()
                    || rookery_facilities::access::holds(
                        &call,
                        &crate::uri(&info.name),
                        Grant::View,
                    )
                    .await?;
                if seen {
                    hosts.push(info);
                }
            }
            Ok(HostList { hosts })
        })
        .summary("The saved hosts the caller may see, and whether each answers.")
        .level(Level::Own);
    methods
        .add("hosts.get", |call: Call, p: HostRef| async move {
            let state = serve(&call)?;
            Ok(api::host_info(&state, &name(&p.host)).await?)
        })
        .summary("One host: its address, its type, and whether it answers.")
        .level(host(Grant::View));
    methods
        .add("hosts.profile", |call: Call, p: HostRef| async move {
            let state = serve(&call)?;
            let profile = answer(&call, setup::profile(state, name(&p.host))).await?;
            Ok(HostProfile { profile })
        })
        .summary("A host's saved profile.")
        .level(host(Grant::Manage));
    methods
        .add("hosts.save", |call: Call, p: HostSave| async move {
            let state = serve(&call)?;
            let name = crate::name_of(&p.host).to_owned();
            let uri = crate::uri(&name);
            let exists = {
                let (state, name) = (state.clone(), name.clone());
                tokio::task::spawn_blocking(move || setup::saved_profile(&state, &name).is_ok())
                    .await
                    .map_err(|e| RpcError::internal(e.to_string()))?
            };
            match exists {
                true if !rookery_facilities::access::holds(&call, &uri, Grant::Manage).await? => {
                    return Err(RpcError::forbidden(format!("changing {name} needs manage on {uri}")));
                }
                false => call.require_admin()?,
                true => {}
            }
            let caller = access(&call, Some(&name)).await?;
            answer(&call, setup::save_profile(state.clone(), name.clone(), caller, p.profile)).await?;
            // The one who made it owns it, which gives them manage on it.
            if !exists && let Ok(user) = call.user_id() {
                call.store().access().set_owner(&uri, user).await?;
                sqlx::query("UPDATE rookery.hosts SET owner = $2 WHERE name = $1")
                    .bind(&name)
                    .bind(user)
                    .execute(call.store().pool())
                    .await
                    .map_err(|e| RpcError::internal(format!("keeping the owner of {name}: {e}")))?;
            }
            call.audit(if exists { "host.change" } else { "host.add" }, Some(&uri), json!({})).await;
            Ok(HostSaved {
                host: api::host_info(&state, &name).await?,
            })
        })
        .summary("Saves a host's profile: a new host, which needs the role admin, or a changed one, which needs manage on it.")
        .level(Level::Own);
    methods
        .add("hosts.remove", |call: Call, p: HostRef| async move {
            let state = serve(&call)?;
            let name = name(&p.host);
            answer(&call, setup::remove_host(state, name.clone())).await?;
            let uri = crate::uri(&name);
            call.store().access().forget(&uri).await?;
            call.audit("host.remove", Some(&uri), json!({})).await;
            Ok(Empty {})
        })
        .summary("Removes a saved host, with its grants. A host whose machine a provisioner made is removed with the action remove.")
        .level(host(Grant::Manage));
    methods
        .add("hosts.rename", |call: Call, p: HostRename| async move {
            let state = serve(&call)?;
            let name = crate::name_of(&p.host).to_owned();
            let mut act = Act {
                token: p.token,
                ..Act::default()
            };
            act.arguments.insert("name".into(), p.to.clone());
            if p.hostname {
                act.arguments.insert("hostname".into(), "yes".into());
            }
            let caller = access(&call, Some(&name)).await?;
            acted(
                &call,
                actions::host_act(state, caller, (name, "rename".into()), body(&act)?),
            )
            .await
        })
        .summary("Renames a host, as a job; its grants and jobs go with the name.")
        .level(host(Grant::Manage));
    methods
        .add("hosts.check", |call: Call, p: HostCheck| async move {
            let state = serve(&call)?;
            let capability = Capability::ALL
                .into_iter()
                .find(|c| c.name() == p.capability)
                .ok_or_else(|| {
                    let names: Vec<_> = Capability::ALL.iter().map(|c| c.name()).collect();
                    RpcError::invalid(format!("no capability {}: one of {}", p.capability, names.join(", ")))
                })?;
            let name = name(&p.host);
            let host_type = {
                let (state, name) = (state.clone(), name.clone());
                tokio::task::spawn_blocking(move || {
                    setup::saved_profile(&state, &name).map(|(_, profile)| rookery_api::profile::host_type(&profile))
                })
                .await
                .map_err(|e| RpcError::internal(e.to_string()))??
            };
            Ok(match rookery_core::host_type::check(&name, host_type, capability) {
                Ok(()) => HostChecked { allowed: true, refusal: None },
                Err(refusal) => HostChecked {
                    allowed: false,
                    refusal: Some(Refusal { why: refusal.why, raise: refusal.raise.into() }),
                },
            })
        })
        .summary("Whether a host's type lets it do something, and if not, why and the action that would.")
        .level(host(Grant::View));
    methods
        .add("hosts.actions", |call: Call, p: HostRef| async move {
            let state = serve(&call)?;
            let name = name(&p.host);
            let caller = access(&call, Some(&name)).await?;
            let actions = answer(&call, actions::host_actions(state, caller, name)).await?;
            Ok(ActionList { actions })
        })
        .summary("What can be done to a host: Rookery's own actions and those its controller lists, each with whether it can run now and the role it needs.")
        .level(host(Grant::View));
    methods
        .add("hosts.act", |call: Call, p: HostAct| async move {
            let state = serve(&call)?;
            let name = crate::name_of(&p.host).to_owned();
            let caller = access(&call, Some(&name)).await?;
            acted(&call, actions::host_act(state, caller, (name, p.action), body(&p.act)?)).await
        })
        .summary("Runs an action of a host: one that changes something as a job, one that only looks at once. Each action names the role it needs on the host.")
        .level(host(Grant::View));
    methods
        .add("hosts.credentials", |call: Call, p: HostRef| async move {
            let state = serve(&call)?;
            answer(&call, setup::credentials(state, name(&p.host))).await
        })
        .summary("The passwords Rookery made for a host's accounts.")
        .level(Level::Admin);
    methods
        .add("installation.actions", |call: Call, _: Empty| async move {
            let _ = serve(&call)?;
            let Json(actions) = actions::list().await;
            Ok(ActionList { actions })
        })
        .summary("What can be done to the installation: roll-outs, key rotation, adopting a host.")
        .level(Level::Admin);
    methods
        .add(
            "installation.act",
            |call: Call, p: InstallationAct| async move {
                let state = serve(&call)?;
                let caller = access(&call, None).await?;
                acted(&call, actions::act(state, caller, p.action, body(&p.act)?)).await
            },
        )
        .summary("Runs an action of the installation, as a job.")
        .level(Level::Admin);
}

/// What a host's controller does, passed through its link.
fn register_controller(methods: &mut Methods) {
    methods
        .add("hosts.stats", |call: Call, p: HostRef| async move {
            pass(
                &call,
                &name(&p.host),
                Capability::Sessions,
                "GET",
                "/v1/stats".into(),
                None,
            )
            .await
        })
        .summary("A host's load, memory, disks and uptime.")
        .level(host(Grant::View));
    methods
        .add("hosts.power", |call: Call, p: HostPower| async move {
            let name = crate::name_of(&p.host).to_owned();
            let target = format!("/v1/power/{}", rookery_api::client::encode(&p.verb));
            let done = pass(
                &call,
                &name,
                Capability::Sessions,
                "POST",
                target,
                Some(json!({})),
            )
            .await?;
            call.audit(
                "host.power",
                Some(&crate::uri(&name)),
                json!({"verb": p.verb}),
            )
            .await;
            Ok(done)
        })
        .summary("Reboots a host, powers it off or suspends it, through its controller.")
        .level(host(Grant::Manage));
    methods
        .add(
            "hosts.desktop.screenshot",
            |call: Call, p: HostRef| async move {
                pass(
                    &call,
                    &name(&p.host),
                    Capability::Desktop,
                    "GET",
                    "/v1/screenshot".into(),
                    None,
                )
                .await
            },
        )
        .summary("A picture of a workstation's desktop, as a PNG in base64.")
        .level(host(Grant::Use));
    methods
        .add("hosts.units.list", |call: Call, p: HostRef| async move {
            pass(
                &call,
                &name(&p.host),
                Capability::Sessions,
                "GET",
                "/v1/units".into(),
                None,
            )
            .await
        })
        .summary("A host's systemd units.")
        .level(host(Grant::View));
    methods
        .add("hosts.units.get", |call: Call, p: HostUnit| async move {
            let target = format!("/v1/units/{}", rookery_api::client::encode(&p.unit));
            pass(
                &call,
                crate::name_of(&p.host),
                Capability::Sessions,
                "GET",
                target,
                None,
            )
            .await
        })
        .summary("One of a host's systemd units.")
        .level(host(Grant::View));
    methods
        .add("hosts.units.act", |call: Call, p: HostUnitAct| async move {
            let name = crate::name_of(&p.host).to_owned();
            let target = format!(
                "/v1/units/{}/{}",
                rookery_api::client::encode(&p.unit),
                rookery_api::client::encode(&p.verb)
            );
            let done = pass(
                &call,
                &name,
                Capability::Sessions,
                "POST",
                target,
                Some(json!({})),
            )
            .await?;
            call.audit(
                "host.unit",
                Some(&crate::uri(&name)),
                json!({"unit": p.unit, "verb": p.verb}),
            )
            .await;
            Ok(done)
        })
        .summary("Starts, stops, restarts, enables or disables one of a host's units.")
        .level(host(Grant::Manage));
    methods
        .add("hosts.logs.read", |call: Call, p: HostQuery| async move {
            let target = format!("/v1/logs{}", query_of(&p.query));
            pass(&call, crate::name_of(&p.host), Capability::Sessions, "GET", target, None).await
        })
        .summary("A host's journal: `unit`, `lines` and the rest as the controller takes them in `query`.")
        .level(host(Grant::Manage));
    methods
        .add("hosts.logs.follow", |call: Call, p: HostQuery| async move {
            let target = format!("/v1/logs{}", query_of(&p.query));
            pass_stream(
                &call,
                crate::name_of(&p.host),
                Capability::Sessions,
                target,
                Carry::Lines,
            )
            .await
        })
        .summary("A host's journal as it is written, a line for each entry.")
        .stream()
        .level(host(Grant::Manage));
    methods
        .add(
            "hosts.processes.list",
            |call: Call, p: HostRef| async move {
                pass(
                    &call,
                    &name(&p.host),
                    Capability::Sessions,
                    "GET",
                    "/v1/processes".into(),
                    None,
                )
                .await
            },
        )
        .summary("The processes on a host.")
        .level(host(Grant::Use));
    methods
        .add(
            "hosts.processes.signal",
            |call: Call, p: HostSignal| async move {
                let name = crate::name_of(&p.host).to_owned();
                let target = format!("/v1/processes/{}/signal", p.pid);
                let body = json!({"signal": p.signal.clone().unwrap_or_else(|| "TERM".into())});
                let done = pass(
                    &call,
                    &name,
                    Capability::Sessions,
                    "POST",
                    target,
                    Some(body),
                )
                .await?;
                call.audit(
                    "host.signal",
                    Some(&crate::uri(&name)),
                    json!({"pid": p.pid}),
                )
                .await;
                Ok(done)
            },
        )
        .summary("Sends a signal to a process on a host.")
        .level(host(Grant::Manage));
    methods
        .add("hosts.services.list", |call: Call, p: HostRef| async move {
            pass(
                &call,
                &name(&p.host),
                Capability::Sessions,
                "GET",
                "/v1/services".into(),
                None,
            )
            .await
        })
        .summary("The services the plugins on a host offer.")
        .level(host(Grant::Use));
    methods
        .add(
            "hosts.services.call",
            |call: Call, p: HostService| async move {
                let target = format!(
                    "/v1/services/{}/{}/{}",
                    rookery_api::client::encode(&p.plugin),
                    rookery_api::client::encode(&p.name),
                    rookery_api::client::encode(&p.method)
                );
                pass(
                    &call,
                    crate::name_of(&p.host),
                    Capability::Sessions,
                    "POST",
                    target,
                    Some(p.params),
                )
                .await
            },
        )
        .summary("Calls a method of a plugin's service on a host.")
        .level(host(Grant::Use));
    methods
        .add(
            "hosts.services.serve",
            |call: Call, p: HostServiceStream| async move {
                let target = format!(
                    "/v1/services/{}/{}/serve",
                    rookery_api::client::encode(&p.plugin),
                    rookery_api::client::encode(&p.name)
                );
                pass_stream(&call, crate::name_of(&p.host), Capability::Sessions, target, Carry::Tagged)
                    .await
            },
        )
        .summary("A stream to a plugin's service on a host; each frame has a byte before it, t for text and b for binary.")
        .stream()
        .level(host(Grant::Use));
}

fn terminal(
    host: &str,
    terminal: &str,
    rest: &str,
    query: &BTreeMap<String, String>,
) -> (String, String) {
    let target = format!(
        "/v1/sessions/{}{rest}{}",
        rookery_api::client::encode(terminal),
        query_of(query)
    );
    (name(host), target)
}

/// What a host's controller said no with: its own failure, 5xx, is the host's (UNAVAILABLE), and
/// the rest the request's.
fn host_refused(status: u16, body: &[u8]) -> RpcError {
    let said = String::from_utf8_lossy(body).into_owned();
    match status {
        403 => RpcError::forbidden(said),
        409 | 412 => RpcError::conflict(said),
        status if status >= 500 => {
            RpcError::unavailable(said).with_details(json!({"host_status": status}))
        }
        _ => RpcError::invalid(said),
    }
}

/// Refused unless the caller may work with this session: `use` on the host reaches the agent's own
/// sessions, and one of another account, or one an action runs, needs `manage`. The host says
/// whose the session is, so an id from anywhere passes only for what its caller may do.
async fn session_guard(call: &Call, name: &str, terminal: &str) -> Result<(), RpcError> {
    if call.is_admin() || matches!(call.caller, rookery_kernel::Caller::Local) {
        return Ok(());
    }
    let (_, target) = self::terminal(name, terminal, "", &BTreeMap::new());
    let Passed(session) = pass(call, name, Capability::Sessions, "GET", target, None).await?;
    let user = session["user"].as_str().unwrap_or_default();
    let action = session["action"]
        .as_str()
        .filter(|action| !action.is_empty());
    if user == "agent" && action.is_none() {
        return Ok(());
    }
    let uri = crate::uri(name);
    if rookery_facilities::access::holds(call, &uri, Grant::Manage).await? {
        return Ok(());
    }
    Err(RpcError::forbidden(match action {
        Some(action) => {
            format!("the session of the action {action} on {name} needs manage on {uri}")
        }
        None => format!("a session as {user} on {name} needs manage on {uri}"),
    }))
}

/// Sessions, files and ports of a host, as the agent account: `hosts.sessions.*` and `hosts.files.*`, `hosts.ports.connect`.
fn register_env(methods: &mut Methods) {
    methods
        .add(
            "hosts.sessions.open",
            |call: Call, p: TerminalOpen| async move {
                let name = name(&p.host);
                // Another account than the agent's operates the host.
                if p.start.user != "agent" {
                    let held = call.is_admin()
                        || rookery_facilities::access::holds(
                            &call,
                            &crate::uri(&name),
                            Grant::Manage,
                        )
                        .await?;
                    if !held {
                        return Err(RpcError::forbidden(format!(
                            "a session as {} needs manage on {}",
                            p.start.user,
                            crate::uri(&name)
                        )));
                    }
                }
                let query = match &p.wait {
                    Some(wait) => format!("?wait={}", rookery_api::client::encode(wait)),
                    None => String::new(),
                };
                let body = serde_json::to_value(&p.start)
                    .map_err(|e| RpcError::internal(e.to_string()))?;
                let opened = pass(
                    &call,
                    &name,
                    Capability::Sessions,
                    "POST",
                    format!("/v1/sessions{query}"),
                    Some(body),
                )
                .await?;
                call.audit(
                    "terminal.open",
                    Some(&crate::uri(&name)),
                    json!({"user": p.start.user}),
                )
                .await;
                Ok(opened)
            },
        )
        .summary(
            "Opens a session on a host: a terminal or a process. The screen is kept on the host.",
        )
        .level(host(Grant::Use));
    methods
        .add("hosts.sessions.list", |call: Call, p: HostRef| async move {
            pass(
                &call,
                &name(&p.host),
                Capability::Sessions,
                "GET",
                "/v1/sessions".into(),
                None,
            )
            .await
        })
        .summary("The sessions on a host.")
        .level(host(Grant::View));
    methods
        .add(
            "hosts.sessions.get",
            |call: Call, p: TerminalRef| async move {
                let (name, target) = terminal(&p.host, &p.terminal, "", &p.query);
                pass(&call, &name, Capability::Sessions, "GET", target, None).await
            },
        )
        .summary("A session on a host.")
        .level(host(Grant::View));
    methods
        .add(
            "hosts.sessions.output",
            |call: Call, p: TerminalRef| async move {
                let (name, target) = terminal(&p.host, &p.terminal, "/output", &p.query);
                session_guard(&call, &name, &p.terminal).await?;
                pass(&call, &name, Capability::Sessions, "GET", target, None).await
            },
        )
        .summary("What a session wrote, from an offset or its last lines. Another account's session, or an action's, needs manage.")
        .level(host(Grant::Use));
    methods
        .add(
            "hosts.sessions.screen",
            |call: Call, p: TerminalRef| async move {
                let (name, target) = terminal(&p.host, &p.terminal, "/screen", &p.query);
                session_guard(&call, &name, &p.terminal).await?;
                pass(&call, &name, Capability::Sessions, "GET", target, None).await
            },
        )
        .summary("A terminal's screen as the host keeps it. Another account's session, or an action's, needs manage.")
        .level(host(Grant::Use));
    methods
        .add("hosts.sessions.close", |call: Call, p: TerminalRef| async move {
            let (name, target) = terminal(&p.host, &p.terminal, "", &p.query);
            session_guard(&call, &name, &p.terminal).await?;
            pass(&call, &name, Capability::Sessions, "DELETE", target, None).await
        })
        .summary("Ends a session on a host, or forgets one that ended. Another account's session, or an action's, needs manage.")
        .level(host(Grant::Use));
    for (method, rest, summary) in [
        (
            "hosts.sessions.input",
            "/input",
            "Types into a session. Another account's session, or an action's, needs manage.",
        ),
        (
            "hosts.sessions.resize",
            "/resize",
            "Gives a terminal another size. Another account's session, or an action's, needs manage.",
        ),
        (
            "hosts.sessions.signal",
            "/signal",
            "Sends a signal to a session's process. Another account's session, or an action's, needs manage.",
        ),
    ] {
        methods
            .add(method, move |call: Call, p: TerminalSend| async move {
                let (name, target) = terminal(&p.host, &p.terminal, rest, &BTreeMap::new());
                session_guard(&call, &name, &p.terminal).await?;
                pass(
                    &call,
                    &name,
                    Capability::Sessions,
                    "POST",
                    target,
                    Some(p.body),
                )
                .await
            })
            .summary(summary)
            .level(host(Grant::Use));
    }
    methods
        .add("hosts.sessions.attach", |call: Call, p: TerminalRef| async move {
            let (name, target) = terminal(&p.host, &p.terminal, "", &p.query);
            session_guard(&call, &name, &p.terminal).await?;
            pass_stream(&call, &name, Capability::Sessions, target, Carry::Tagged).await
        })
        .summary("A session's output as it comes and its input, as a stream: each frame has a byte before it, b for the terminal's bytes and t for the host's JSON, such as a size to take or how it exited. Another account's session, or an action's, needs manage.")
        .stream()
        .level(host(Grant::Use));
    methods
        .add("hosts.files.read", |call: Call, p: FileRef| async move {
            let name = name(&p.host);
            let state = serve(&call)?;
            let remote = api::controller(&state, &name, Capability::Sessions).await?;
            let link = remote
                .link
                .as_ref()
                .ok_or_else(|| RpcError::unavailable("no link"))?;
            let target = format!("/v1/files?path={}", rookery_api::client::encode(&p.path));
            let answer = link
                .send("GET", &target, &[], &[])
                .await
                .map_err(api::Fail::from)?;
            match answer.status {
                404 => return Err(RpcError::not_found(format!("no file {}", p.path))),
                status if status >= 400 => return Err(host_refused(status, &answer.body)),
                _ => {}
            }
            let header = |key: &str| {
                answer
                    .headers
                    .get(key)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
            };
            Ok(Passed(json!({
                "base64": rookery_api::b64(&answer.body),
                "content_type": header("content-type").unwrap_or_default(),
                "etag": header("etag"),
            })))
        })
        .summary(
            "A file on a host, in base64 with its content_type and etag, inside the host's roots.",
        )
        .level(host(Grant::Use));
    methods
        .add("hosts.files.write", |call: Call, p: FileWrite| async move {
            let name = name(&p.host);
            let state = serve(&call)?;
            let remote = api::controller(&state, &name, Capability::Sessions).await?;
            let link = remote
                .link
                .as_ref()
                .ok_or_else(|| RpcError::unavailable("no link"))?;
            let bytes = rookery_api::unb64(&p.base64)
                .ok_or_else(|| RpcError::invalid("base64: not base64"))?;
            // With no etag a file that is there is replaced.
            let target = format!(
                "/v1/files?path={}{}",
                rookery_api::client::encode(&p.path),
                if p.etag.is_none() {
                    "&overwrite=true"
                } else {
                    ""
                }
            );
            let mut headers = vec![("content-type", "application/octet-stream")];
            if let Some(etag) = &p.etag {
                headers.push(("if-match", etag.as_str()));
            }
            let answer = link
                .send("PUT", &target, &headers, &bytes)
                .await
                .map_err(api::Fail::from)?;
            if answer.status == 412 {
                return Err(RpcError::conflict(format!(
                    "{} changed since it was read",
                    p.path
                )));
            }
            if answer.status >= 400 {
                return Err(host_refused(answer.status, &answer.body));
            }
            Ok(Passed(
                serde_json::from_slice(&answer.body).unwrap_or(json!({})),
            ))
        })
        .summary("Writes a file on a host, inside its roots; with etag only if it is still at it.")
        .level(host(Grant::Use));
    methods
        .add("hosts.files.delete", |call: Call, p: FileRef| async move {
            let target = format!("/v1/files?path={}", rookery_api::client::encode(&p.path));
            pass(
                &call,
                &name(&p.host),
                Capability::Sessions,
                "DELETE",
                target,
                None,
            )
            .await
        })
        .summary("Removes a file on a host, inside its roots.")
        .level(host(Grant::Use));
    methods
        .add(
            "hosts.ports.connect",
            |call: Call, p: PortConnect| async move {
                pass_stream(
                    &call,
                    &name(&p.host),
                    Capability::Sessions,
                    format!("/v1/ports/{}", p.port),
                    Carry::Bytes,
                )
                .await
            },
        )
        .summary("A byte stream to a TCP port on a host's loopback.")
        .stream()
        .level(host(Grant::Use));
    methods
        .add("resources.connect", |call: Call, p: EndpointConnect| async move {
            let state = serve(&call)?;
            let connection = crate::serve::connect::connect(state, (p.id, p.endpoint)).await?;
            let stream = call.conn()?.streams().open(Initiator::Peer)?;
            let id = stream.id();
            tokio::spawn(crate::serve::tunnel(stream, connection));
            Ok(StreamOpened { stream: id, offset: None })
        })
        .summary("A byte stream to an endpoint a resource's provisioner reported, by its name or its port.")
        .stream()
        .level(Level::Admin);
}

/// The messages hosts keep, taken in by the leader from their channels.
fn register_messages(methods: &mut Methods) {
    methods
        .add("hosts.messages.list", |call: Call, p: MessageQuery| async move {
            let state = serve(&call)?;
            let host = p.host.as_deref().map(name);
            let key = host.as_ref().map(|host| {
                let (state, host) = (state.clone(), host.clone());
                (state, host)
            });
            let filter_host = match key {
                Some((state, host)) => Some(
                    tokio::task::spawn_blocking(move || api::host_key(&state.paths, &host))
                        .await
                        .map_err(|e| RpcError::internal(e.to_string()))?,
                ),
                None => None,
            };
            let limit = p.limit.unwrap_or(100).clamp(1, 1000);
            let (topic, done, before) = (p.topic.clone(), p.done, p.before);
            let found = {
                let state = state.clone();
                tokio::task::spawn_blocking(move || {
                    let filter = rookery_core::store::Filter {
                        host: filter_host.as_deref(),
                        topic: topic.as_deref(),
                        before,
                        done,
                        limit,
                    };
                    let found = state.store.inbox(&filter)?;
                    let named: Vec<(rookery_core::store::Stored, String)> = found
                        .0
                        .into_iter()
                        .map(|stored| {
                            let name = state
                                .store
                                .label_of(rookery_core::store::kind::PROFILE, rookery_core::store::OWN, &stored.host)
                                .ok()
                                .flatten()
                                .unwrap_or_else(|| stored.host.clone());
                            (stored, name)
                        })
                        .collect();
                    Ok::<_, rookery_core::store::Error>((named, found.1))
                })
                .await
                .map_err(|e| RpcError::internal(e.to_string()))?
                .map_err(api::Fail::from)?
            };
            let mut messages = vec![];
            for (stored, host) in found.0 {
                let seen = call.is_admin()
                    || rookery_facilities::access::holds(&call, &crate::uri(&host), Grant::View).await?;
                if seen {
                    messages.push(HostMessage { message: stored.message, host, done: stored.done, seq: stored.seq });
                }
            }
            Ok(MessageList { messages, next: found.1 })
        })
        .summary("The messages hosts kept on their channels, newest first, of the hosts the caller sees.")
        .level(Level::Own);
    for (method, done, summary) in [
        ("hosts.messages.done", true, "Marks a host's message done."),
        (
            "hosts.messages.reopen",
            false,
            "Marks a host's message open again.",
        ),
    ] {
        methods
            .add(method, move |call: Call, p: MessageRef| async move {
                let state = serve(&call)?;
                let host = name(&p.host);
                let changed = tokio::task::spawn_blocking(move || {
                    let key = api::host_key(&state.paths, &host);
                    state.store.set_done(&key, p.id, done)
                })
                .await
                .map_err(|e| RpcError::internal(e.to_string()))?
                .map_err(api::Fail::from)?;
                match changed {
                    true => Ok(Empty {}),
                    false => Err(RpcError::not_found("no such message")),
                }
            })
            .summary(summary)
            .level(host(Grant::Manage));
    }
    methods
        .add(
            "hosts.channel.listen",
            |call: Call, p: HostQuery| async move {
                let target = format!("/v1/channel{}", query_of(&p.query));
                pass_stream(
                    &call,
                    &name(&p.host),
                    Capability::Sessions,
                    target,
                    Carry::Lines,
                )
                .await
            },
        )
        .summary(
            "A host's channel as its messages come, a JSON line each, from `since` in `query`.",
        )
        .stream()
        .level(host(Grant::View));
}

fn register_keys(methods: &mut Methods) {
    methods
        .add("keys.list", |call: Call, _: Empty| async move {
            answer(&call, keys::list(serve(&call)?)).await
        })
        .summary(
            "The key ring: the keys Rookery logs in to hosts with, and the hosts that take each.",
        )
        .level(Level::Admin);
    methods
        .add("keys.add", |call: Call, p: KeyAdd| async move {
            let private = p.private.map(String::into_bytes).unwrap_or_default();
            let added = answer(&call, keys::add(serve(&call)?, p.name.clone(), private)).await?;
            call.audit("key.add", None, json!({"name": p.name})).await;
            Ok(added)
        })
        .summary("Adds a key to the ring: one given, or a new one.")
        .level(Level::Admin);
    methods
        .add("keys.remove", |call: Call, p: KeyRef| async move {
            answer(&call, keys::remove(serve(&call)?, p.name.clone())).await?;
            call.audit("key.remove", None, json!({"name": p.name}))
                .await;
            Ok(Empty {})
        })
        .summary("Removes a key from the ring, when no host logs in with it.")
        .level(Level::Admin);
    methods
        .add("keys.rotate", |call: Call, p: KeyRef| async move {
            acted(&call, keys::rotate(serve(&call)?, p.name)).await
        })
        .summary("Replaces a key on every host that takes it, as a job.")
        .level(Level::Admin);
    methods
        .add("keys.trust", |call: Call, p: HostRef| async move {
            let state = serve(&call)?;
            let caller = access(&call, Some(&name(&p.host))).await?;
            acted(&call, actions::host_act(state, caller, (name(&p.host), "trust-key".into()), body(&Act::default())?)).await
        })
        .summary("Puts the ring's key in a host's authorized keys, logging in once as its SSH user can, as a job.")
        .level(Level::Admin);
}

fn register_provisioning(methods: &mut Methods) {
    methods
        .add("provisioners.list", |call: Call, _: Empty| async move {
            answer(&call, provisioners::list(serve(&call)?)).await
        })
        .summary("The provisioners under names of their own.")
        .level(Level::Admin);
    methods
        .add("provisioners.types", |call: Call, _: Empty| async move {
            answer(&call, provisioners::types(serve(&call)?)).await
        })
        .summary("The types of provisioner the plugins bring.")
        .level(Level::Admin);
    methods
        .add(
            "provisioners.get",
            |call: Call, p: ProvisionerGet| async move {
                answer(
                    &call,
                    provisioners::provisioner(serve(&call)?, p.name, p.given),
                )
                .await
            },
        )
        .summary("A provisioner and whether it can be used now.")
        .level(Level::Admin);
    methods
        .add(
            "provisioners.save",
            |call: Call, p: ProvisionerSave| async move {
                answer(
                    &call,
                    provisioners::save(serve(&call)?, p.name, body(&p.saved)?),
                )
                .await
            },
        )
        .summary("Registers a provisioner under a name of its own, or changes its settings.")
        .level(Level::Admin);
    methods
        .add("provisioners.remove", |call: Call, p: Named| async move {
            answer(&call, provisioners::remove(serve(&call)?, p.name))
                .await
                .map(|_| Empty {})
        })
        .summary("Removes a provisioner that made nothing that is kept.")
        .level(Level::Admin);
    methods
        .add("provisioners.actions", |call: Call, p: Named| async move {
            let actions = answer(
                &call,
                provisioners::provisioner_actions(serve(&call)?, p.name),
            )
            .await?;
            Ok(ActionList { actions })
        })
        .summary("What a provisioner can do.")
        .level(Level::Admin);
    methods
        .add(
            "provisioners.act",
            |call: Call, p: ProvisionerAct| async move {
                let caller = access(&call, None).await?;
                acted(
                    &call,
                    provisioners::provisioner_act(
                        serve(&call)?,
                        caller,
                        (p.name, p.action),
                        body(&p.act)?,
                    ),
                )
                .await
            },
        )
        .summary("Runs an action of a provisioner.")
        .level(Level::Admin);
    methods
        .add(
            "resources.list",
            |call: Call, p: resources::Filter| async move {
                answer(&call, resources::list(serve(&call)?, p)).await
            },
        )
        .summary("What the provisioners made: machines for hosts and the rest.")
        .level(Level::Admin);
    methods
        .add("resources.get", |call: Call, p: ResourceRef| async move {
            answer(&call, resources::get(serve(&call)?, p.id)).await
        })
        .summary("A resource, as its provisioner sees it now.")
        .level(Level::Admin);
    methods
        .add(
            "resources.create",
            |call: Call, p: rookery_api::NewResource| async move {
                answer(&call, resources::create(serve(&call)?, body(&p)?)).await
            },
        )
        .summary("Asks a provisioner for a resource, as a job.")
        .level(Level::Admin);
    methods
        .add(
            "resources.actions",
            |call: Call, p: ResourceRef| async move {
                let actions = answer(&call, resources::actions(serve(&call)?, p.id)).await?;
                Ok(ActionList { actions })
            },
        )
        .summary("What can be done to a resource.")
        .level(Level::Admin);
    methods
        .add("resources.act", |call: Call, p: ResourceAct| async move {
            let caller = access(&call, None).await?;
            acted(
                &call,
                resources::act(serve(&call)?, caller, (p.id, p.action), body(&p.act)?),
            )
            .await
        })
        .summary("Runs an action of a resource.")
        .level(Level::Admin);
    methods
        .add(
            "resources.change",
            |call: Call, p: ResourceChangeOf| async move {
                let caller = access(&call, None).await?;
                acted(
                    &call,
                    resources::change(serve(&call)?, caller, p.id, body(&p.change)?),
                )
                .await
            },
        )
        .summary("Changes a resource's arguments or specification, as a job.")
        .level(Level::Admin);
    methods
        .add(
            "resources.remove",
            |call: Call, p: ResourceRemove| async move {
                let caller = access(&call, None).await?;
                let token = resources::Token::with(p.token);
                acted(&call, resources::remove(serve(&call)?, caller, p.id, token)).await
            },
        )
        .summary("Removes a resource with its provisioner, as a job.")
        .level(Level::Admin);
    methods
        .add("resources.logs", |call: Call, p: ResourceLogs| async move {
            let query = resources::LogQuery::with(p.tail);
            let text = answer(&call, resources::logs(serve(&call)?, p.id, query)).await?;
            Ok(Text { text })
        })
        .summary("A resource's log as it is now.")
        .level(Level::Admin);
    methods
        .add(
            "resources.follow",
            |call: Call, p: ResourceLogs| async move {
                let state = serve(&call)?;
                let stream = call.conn()?.streams().open(Initiator::Peer)?;
                let id = stream.id();
                tokio::spawn(resources::follow(
                    state,
                    p.id,
                    p.tail.unwrap_or(100),
                    stream,
                ));
                Ok(StreamOpened {
                    stream: id,
                    offset: None,
                })
            },
        )
        .summary("A resource's log as it comes, then a line with how it ended.")
        .stream()
        .level(Level::Admin);
}

fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn parsed<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value).map_err(|e| RpcError::invalid(format!("value: {e}")))
}

/// The installation's secrets, settings and registries, as Rookery has had them: a host's SSH and
/// sudo passwords, a registry's login and a provisioner's secrets belong to what they are for.
fn register_installation(methods: &mut Methods) {
    methods
        .add("installation.secrets.list", |call: Call, _: Empty| async move {
            answer(&call, setup::secrets(serve(&call)?)).await
        })
        .summary("The names of the installation's secrets: its own, and those of hosts, registries and provisioners.")
        .level(Level::Admin);
    methods
        .add("installation.secrets.set", |call: Call, p: NamedValue| async move {
            let value = parsed(json!({"value": text(&p.value)}))?;
            answer(&call, setup::set_secret(serve(&call)?, p.name.clone(), value)).await?;
            call.audit("secret.set", None, json!({"name": p.name})).await;
            Ok(Empty {})
        })
        .summary("Sets a secret: sudo/HOST and ssh/HOST for a host's passwords, registry/NAME for a registry's login, or one of a name of its own.")
        .level(Level::Admin);
    methods
        .add(
            "installation.secrets.rename",
            |call: Call, p: NamedValue| async move {
                let to = parsed(json!({"name": text(&p.value)}))?;
                answer(&call, setup::rename_secret(serve(&call)?, p.name, to)).await?;
                Ok(Empty {})
            },
        )
        .summary("Renames a secret of a name of its own; value is the new name.")
        .level(Level::Admin);
    methods
        .add(
            "installation.secrets.remove",
            |call: Call, p: Named| async move {
                answer(&call, setup::remove_secret(serve(&call)?, p.name.clone())).await?;
                call.audit("secret.remove", None, json!({"name": p.name}))
                    .await;
                Ok(Empty {})
            },
        )
        .summary("Removes a secret.")
        .level(Level::Admin);
    methods
        .add(
            "installation.settings.list",
            |call: Call, _: Empty| async move {
                let caller = access(&call, None).await?;
                answer(&call, setup::settings(serve(&call)?, caller)).await
            },
        )
        .summary(
            "The installation's settings, with what each means; secret ones without their value.",
        )
        .level(Level::Admin);
    methods
        .add("installation.settings.set", |call: Call, p: NamedValue| async move {
            let caller = access(&call, None).await?;
            let value = parsed(json!({"value": text(&p.value)}))?;
            answer(&call, setup::set_setting(serve(&call)?, caller, p.name.clone(), value)).await?;
            call.audit("setting.set", None, json!({"name": p.name})).await;
            Ok(Empty {})
        })
        .summary("Sets one of the installation's settings, or with an empty value takes it back to its default. Those that are not open are set over the socket alone.")
        .level(Level::Admin);
    methods
        .add("registries.list", |call: Call, _: Empty| async move {
            answer(&call, setup::registries(serve(&call)?)).await
        })
        .summary("The registries hosts are given logins and mirrors of; connections of the type registry.")
        .level(Level::Admin);
    methods
        .add("registries.save", |call: Call, p: NamedValue| async move {
            answer(&call, setup::save_registry(serve(&call)?, p.name, p.value)).await?;
            Ok(Empty {})
        })
        .summary("Saves a registry: value is the registry, with its type and url.")
        .level(Level::Admin);
    methods
        .add(
            "registries.rename",
            |call: Call, p: NamedValue| async move {
                let to = parsed(json!({"name": text(&p.value)}))?;
                answer(&call, setup::rename_registry(serve(&call)?, p.name, to)).await?;
                Ok(Empty {})
            },
        )
        .summary("Renames a registry, with its login; value is the new name.")
        .level(Level::Admin);
    methods
        .add("registries.remove", |call: Call, p: Named| async move {
            answer(&call, setup::remove_registry(serve(&call)?, p.name)).await?;
            Ok(Empty {})
        })
        .summary("Removes a registry with its login.")
        .level(Level::Admin);
    methods
        .add("registries.check", |call: Call, p: Named| async move {
            answer(&call, setup::check_registry(serve(&call)?, p.name)).await
        })
        .summary("Whether a registry answers, and takes its login.")
        .level(Level::Admin);
}

fn register_setup(methods: &mut Methods) {
    methods
        .add("setup.catalogue", |call: Call, _: Empty| async move {
            answer(&call, setup::catalogue(serve(&call)?)).await
        })
        .summary("The set-up flow the CLI's prompts and a shell's views read: the fields of a profile and the features the plugins offer.")
        .level(Level::Admin);
    methods
        .add(
            "setup.generate",
            |call: Call, p: setup::Generate| async move {
                answer(&call, setup::generate(serve(&call)?, p)).await
            },
        )
        .summary("Makes something up for a profile, such as a password or a key.")
        .level(Level::Admin);
    methods
        .add("plugins.installed", |call: Call, _: Empty| async move {
            answer(&call, setup::plugins(serve(&call)?)).await
        })
        .summary("The plugins installed on the server, with their features and provisioners. plugins.list is the plugin system's, with client halves.")
        .level(Level::Admin);
    methods
        .add("plugins.install", |call: Call, p: PluginSpec| async move {
            let report = answer(&call, setup::add_plugin(serve(&call)?, p)).await?;
            call.audit("plugin.install", None, json!({})).await;
            Ok(report)
        })
        .summary(
            "Installs a plugin from the plugin registries, an egg's address or a git repository.",
        )
        .level(Level::Admin);
    methods
        .add("plugins.upload", |call: Call, p: PluginUpload| async move {
            let egg = rookery_api::unb64(&p.base64)
                .ok_or_else(|| RpcError::invalid("base64: not base64"))?;
            answer(&call, setup::upload_plugin(serve(&call)?, egg)).await
        })
        .summary("Installs a plugin from an egg the caller sends.")
        .level(Level::Admin);
    methods
        .add("plugins.remove", |call: Call, p: PluginRef| async move {
            let report = answer(&call, setup::remove_plugin(serve(&call)?, p.id.clone())).await?;
            call.audit("plugin.remove", None, json!({"plugin": p.id}))
                .await;
            Ok(report)
        })
        .summary("Removes one of the server's plugins.")
        .level(Level::Admin);
    methods
        .add("plugins.check", |call: Call, _: Empty| async move {
            answer(&call, setup::check_plugins(serve(&call)?)).await
        })
        .summary("Which plugins have a newer version.")
        .level(Level::Admin);
    methods
        .add(
            "plugins.update",
            |call: Call, p: PluginUpdateSpec| async move {
                answer(&call, setup::update_plugins(serve(&call)?, body(&p)?)).await
            },
        )
        .summary("Updates plugins to their newest versions.")
        .level(Level::Admin);
    methods
        .add("plugins.registry.list", |call: Call, _: Empty| async move {
            answer(&call, setup::plugin_registries(serve(&call)?)).await
        })
        .summary("The plugin registries, with their keys.")
        .level(Level::Admin);
    methods
        .add(
            "plugins.registry.add",
            |call: Call, p: PluginRegistry| async move {
                let caller = access(&call, None).await?;
                answer(&call, setup::add_plugin_registry(serve(&call)?, caller, p)).await
            },
        )
        .summary("Adds a plugin registry.")
        .level(Level::Admin);
    methods
        .add(
            "plugins.registry.remove",
            |call: Call, p: setup::RegistryLocation| async move {
                answer(&call, setup::remove_plugin_registry(serve(&call)?, p)).await
            },
        )
        .summary("Removes a plugin registry.")
        .level(Level::Admin);
    methods
        .add("plugins.reload", |call: Call, _: Empty| async move {
            let state = serve(&call)?;
            state.refresh_now(false).await;
            Ok(Empty {})
        })
        .summary("Reads the plugins and the saved hosts again.")
        .level(Level::Admin);
}
