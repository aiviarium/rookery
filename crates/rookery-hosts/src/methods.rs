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
use crate::serve::{Serve, actions, auth, keys, provisioners, resources, setup};

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
    tokio::spawn(crate::serve::pipe(upstream, stream));
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
    register_provisioning(methods);
    register_setup(methods);
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
        .level(host(Grant::View));
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
                let _ = sqlx::query("UPDATE rookery.hosts SET owner = $2 WHERE name = $1")
                    .bind(&name)
                    .bind(user)
                    .execute(call.store().pool())
                    .await;
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
        .level(host(Grant::View));
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
            pass_stream(&call, crate::name_of(&p.host), Capability::Sessions, target).await
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
        .level(host(Grant::View));
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
                pass_stream(&call, crate::name_of(&p.host), Capability::Sessions, target).await
            },
        )
        .summary("A stream to a plugin's service on a host.")
        .stream()
        .level(host(Grant::Use));
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
        .add("plugins.list", |call: Call, _: Empty| async move {
            answer(&call, setup::plugins(serve(&call)?)).await
        })
        .summary("The plugins installed on the server.")
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
