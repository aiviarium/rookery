//! The routes of the provisioners of plugins: each as a subject with its
//! page and its actions. What they make is a resource (see
//! [`super::resources`]). The provisioners run here, with the server's
//! secrets. Looking needs no more than `view`; an action needs its own role.

use super::{
    Serve,
    actions::{admit, answer, parse, refused},
    api::Acted,
    api::{Answer, Bytes, Fail, Json, caller, internal},
    auth::Access,
};
use futures_util::StreamExt;
use rookery_api::{Action, ProvisionerInfo, ProvisionerState, Resource, SavedProvisioner, code};
use rookery_core::{
    provision, provisioner,
    registry::Store,
    runner::{Request, Subject, Work},
};
use std::sync::Arc;
use std::{collections::BTreeMap, io::Write};

/// How many provisioners a list asks at once, so that one that does not
/// answer holds the list for one turn of the queue and no more.
const ASKED_AT_ONCE: usize = 4;

fn bad(error: impl std::fmt::Display) -> Fail {
    Fail::new(code::BAD_REQUEST, error.to_string())
}

fn missing(what: impl std::fmt::Display) -> Fail {
    Fail::new(code::NOT_FOUND, what.to_string())
}

/// The server's own registries and secrets.
fn store(state: &Serve) -> Store {
    Store::own(state.store.clone())
}

/// Run a provisioner's work off the runtime: it waits for the provisioner.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> rookery_core::Result<T> + Send + 'static,
) -> Answer<Result<T, String>> {
    tokio::task::spawn_blocking(move || work().map_err(|error| error.to_string()))
        .await
        .map_err(internal)
}

/// A registered provisioner by its name.
fn known(state: &Serve, store: &Store, name: &str) -> Answer<ProvisionerInfo> {
    provision::list(&state.paths, store)
        .into_iter()
        .find(|known| known.name == name || known.id == name)
        .ok_or_else(|| {
            missing(format!(
                "No provisioner {name}. See: rookery provisioner list"
            ))
        })
}

pub async fn list(state: Arc<Serve>) -> Answer<Json<Vec<ProvisionerInfo>>> {
    let registries = store(&state);
    let options = (state.provision)();
    let mut infos = provision::list(&state.paths, &registries);
    // One that cannot be used here is not asked at all.
    for info in infos.iter_mut().filter(|info| !info.problem.is_empty()) {
        info.state = "unusable".into();
        info.message = info.problem.clone();
    }
    // What each says of itself, a few at a time and within one deadline for
    // the whole list, so a page needs no call per name and a provisioner
    // that hangs does not hold it. A task that ends without an answer, as a
    // panicked one does, is shown as unusable rather than left blank.
    let deadline = tokio::time::Instant::now() + options.quick;
    let asking: Vec<_> = infos
        .iter()
        .filter(|info| info.problem.is_empty())
        .map(|info| {
            let (paths, registries, name) =
                (state.paths.clone(), registries.clone(), info.name.clone());
            let options = (state.provision)();
            async move {
                let asked = name.clone();
                let answered = tokio::task::spawn_blocking(move || {
                    provisioner::status(&paths, &registries, &asked, &options)
                })
                .await;
                (name, answered)
            }
        })
        .collect();
    let mut asking = Box::pin(futures_util::stream::iter(asking).buffer_unordered(ASKED_AT_ONCE));
    let mut waiting = Box::pin(tokio::time::sleep_until(deadline));
    loop {
        tokio::select! {
            _ = &mut waiting => break,
            next = asking.next() => match next {
                Some((name, Ok((state, message)))) => {
                    if let Some(info) = infos.iter_mut().find(|info| info.name == name) {
                        info.state = state;
                        info.message = message;
                    }
                }
                Some((name, Err(error))) => {
                    if let Some(info) = infos.iter_mut().find(|info| info.name == name) {
                        info.state = "unusable".into();
                        info.message = format!("{name} could not be asked: {error}");
                    }
                }
                None => break,
            },
        }
    }
    // Whatever the deadline left is what `provisioners.get` says of one whose
    // status ran out of time.
    for info in infos
        .iter_mut()
        .filter(|info| info.problem.is_empty() && info.state.is_empty())
    {
        info.state = "unreachable".into();
        info.message = provision::unanswered(&info.name, "status", options.quick);
    }
    Ok(Json(infos))
}

pub async fn types(state: Arc<Serve>) -> Answer<Json<Vec<ProvisionerInfo>>> {
    Ok(Json(provision::types(&state.paths)))
}

pub async fn save(state: Arc<Serve>, name: String, body: Bytes) -> Answer<Json<ProvisionerInfo>> {
    let saved: SavedProvisioner = serde_json::from_slice(&body).map_err(bad)?;
    provisioner::register(&state.paths, &name, &saved).map_err(bad)?;
    let registries = store(&state);
    Ok(Json(known(&state, &registries, &name)?))
}

pub async fn remove(state: Arc<Serve>, name: String) -> Answer<Json<rookery_api::Done>> {
    let registries = store(&state);
    let info = known(&state, &registries, &name)?;
    let held = provisioner::remove_entry(&state.paths, &registries, &name)
        .map_err(|error| Fail::new(code::CONFLICT, error.to_string()))?;
    if held.is_empty() {
        return Ok(Json(rookery_api::Done { ok: true }));
    }
    // What it holds is what has to be removed first, so the caller is given
    // the resources themselves, not only their ids. The plugins are read
    // once for all of them.
    let plugins = provision::list_plugins(&state.paths);
    let resources: Vec<Resource> = held
        .iter()
        .map(|kept| super::resources::described(&state, kept, &plugins))
        .collect();
    Err(
        Fail::new(code::CONFLICT, provisioner::held_message(&info.name, &held))
            .with(serde_json::json!({ "resources": resources })),
    )
}

pub async fn provisioner(
    state: Arc<Serve>,
    name: String,
    given: BTreeMap<String, String>,
) -> Answer<Json<ProvisionerState>> {
    let registries = store(&state);
    known(&state, &registries, &name)?;
    let (paths, options) = (state.paths.clone(), (state.provision)());
    let found = blocking(move || provisioner::state(&paths, &registries, &name, &given, &options))
        .await?
        .map_err(internal)?;
    Ok(Json(found))
}

pub async fn provisioner_actions(state: Arc<Serve>, name: String) -> Answer<Json<Vec<Action>>> {
    let registries = store(&state);
    known(&state, &registries, &name)?;
    Ok(Json(
        provisioner::actions(&state.paths, &registries, &name).map_err(internal)?,
    ))
}

pub async fn provisioner_act(
    state: Arc<Serve>,
    access: Option<Access>,
    (name, action): (String, String),
    body: Bytes,
) -> Answer<Json<Acted>> {
    let act = parse(&body)?;
    let registries = store(&state);
    known(&state, &registries, &name)?;
    let actions = provisioner::actions(&state.paths, &registries, &name).map_err(internal)?;
    let Some(found) = actions.iter().find(|known| known.name == action) else {
        let names: Vec<&str> = actions.iter().map(|known| known.name.as_str()).collect();
        return Err(missing(format!(
            "{name} has no action {action}. Its actions: {}",
            names.join(", ")
        )));
    };
    admit(&state, &caller(access), found, &act)?;
    let acting = provisioner::prepare(&state.paths, &registries, &name, &action, &act.arguments)
        .map_err(bad)?;
    let (paths, options) = (state.paths.clone(), (state.provision)());
    if !acting.changes() {
        let answered = blocking(move || acting.run(&paths, &options, &mut std::io::sink()))
            .await?
            .map_err(|error| Fail::new(code::UNREACHABLE, error))?;
        return Ok(answer(Acted {
            result: Some(answered.value()),
            ..Acted::default()
        }));
    }
    let mut request = Request::new(Subject::Provisioner(name), &action);
    request.arguments = rookery_api::Argument::shown(&found.arguments, &act.arguments);
    request.token = act.token;
    request.role = found.role;
    request.secrets = acting.secrets();
    let work = Work::Function(Box::new(move |out| {
        match acting.run(&paths, &options, out) {
            Ok(answered) => {
                for line in answered.lines() {
                    let _ = writeln!(out, "{line}");
                }
                true
            }
            Err(error) => {
                let _ = writeln!(out, "✗ {error}");
                false
            }
        }
    }));
    let job = state.runner.start(request, work).await.map_err(refused)?;
    Ok(answer(Acted {
        job: Some(job),
        ..Acted::default()
    }))
}
