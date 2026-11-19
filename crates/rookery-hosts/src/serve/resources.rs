//! The routes of resources: what provisioners made for rookery, a host's
//! machine among them. rookery's records answer the list; one resource is
//! asked of its provisioner. Making, changing and removing one are jobs of
//! the resource as a subject, holding its lock, and a token given again
//! answers with the job the first call started.

use super::{
    Serve,
    actions::{admit, answer, parse, refused},
    api::Acted,
    api::{Answer, Bytes, Fail, Json, Said, caller, internal, turned_down},
    auth::Access,
};
use crate::runner::Reserved;
use rookery_api::{Act, Action, NewResource, Resource, ResourceChange, code};
use rookery_core::{
    host::Host,
    paths::Paths,
    provisioner::Answered,
    registry::Store,
    resource::{self, Asked, Given},
    runner::{Request, Work},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{io::Write, sync::Arc, time::Duration};

fn bad(error: impl std::fmt::Display) -> Fail {
    Fail::new(code::BAD_REQUEST, error.to_string())
}

/// A refusal of the core as an answer: a missing resource is 404, one that
/// is busy 409, anything else a request that does not fit.
pub(crate) fn refusal(error: impl Into<Said>) -> Fail {
    let said = error.into();
    let message = said.message.clone();
    if said.refused {
        turned_down(said)
    } else if message.starts_with("No resource ") || message.starts_with("No provisioner ") {
        Fail::new(code::NOT_FOUND, message)
    } else if message.starts_with("Something else is being done to")
        || message.contains(" already, ")
    {
        Fail::new(code::CONFLICT, message)
    } else {
        bad(message)
    }
}

fn store(state: &Serve) -> Store {
    Store::own(state.store.clone())
}

/// Run work of the core off the runtime: it waits for the provisioner.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> rookery_core::Result<T> + Send + 'static,
) -> Answer<Result<T, Said>> {
    tokio::task::spawn_blocking(move || work().map_err(Said::from))
        .await
        .map_err(internal)
}

/// A resource by its id, as its record has it.
fn found(state: &Serve, id: &str) -> Answer<resource::Kept> {
    resource::find(&state.paths, id).map_err(refusal)
}

/// What is said of a resource, with the plugins of the installation read
/// once for a listing of several.
pub(crate) fn described(
    state: &Serve,
    kept: &resource::Kept,
    plugins: &[rookery_core::plugins::Plugin],
) -> Resource {
    let busy = kept.record.external_id.is_empty() && kept.busy(&state.paths);
    resource::info(&state.paths, kept, plugins, busy)
}

/// A profile this call saved, and what it replaced, so that a call that ends
/// without a machine leaves the host as it was.
struct Saved {
    place: rookery_core::profiles::Place,
    /// The profile it had, or none when the host is new.
    before: Option<Value>,
    /// The host's lock, held from the save until the job that makes the
    /// machine takes it.
    lock: Option<rookery_core::provision::Lock>,
}

/// Put a host back as it was before a call that saved its profile: the
/// profile it had, or none at all.
fn restore(state: &Arc<Serve>, saved: Option<&Saved>) {
    let Some(saved) = saved else { return };
    match &saved.before {
        Some(profile) => {
            let _ = saved.place.save(&state.paths, profile);
        }
        None => {
            let _ = rookery_core::profiles::remove(&state.paths, &saved.place);
        }
    }
    state.forget(&saved.place.argument());
    state.refresh(false);
}

/// How a host is reached, to tell whether making its machine changed it.
fn reached(paths: &Paths, host: &str) -> Value {
    rookery_core::profiles::Place::own(paths, host)
        .and_then(|place| Host::load_at(paths, place))
        .map(|host| host.config["access"].clone())
        .unwrap_or_default()
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct Filter {
    /// Only those of this provisioner, by its name.
    provisioner: Option<String>,
    /// Only those of this kind, such as `machine` or `deployment`.
    kind: Option<String>,
    /// Only the machine of this saved host.
    host: Option<String>,
}

pub async fn list(state: Arc<Serve>, filter: Filter) -> Answer<Json<Vec<Resource>>> {
    let listed = resource::listed(
        &state.paths,
        filter.provisioner.as_deref(),
        filter.kind.as_deref(),
        filter.host.as_deref(),
    )
    .map_err(internal)?;
    Ok(Json(listed))
}

pub async fn get(state: Arc<Serve>, id: String) -> Answer<Json<Resource>> {
    found(&state, &id)?;
    let (paths, registries, options) = (state.paths.clone(), store(&state), (state.provision)());
    let seen = blocking(move || resource::status(&paths, &registries, &id, &options))
        .await?
        .map_err(refusal)?;
    // What it said, such as endpoints that moved, is told to followers now.
    state.notice();
    Ok(Json(seen))
}

/// A resource asked for, and the job that makes it.
#[derive(Serialize, schemars::JsonSchema)]
pub struct Created {
    pub resource: Resource,
    pub job: rookery_proto::jobs::JobInfo,
}

pub async fn create(
    state: Arc<Serve>,
    access: Option<Access>,
    body: Bytes,
) -> Answer<Json<Created>> {
    let mut asked: NewResource = serde_json::from_slice(&body).map_err(bad)?;
    let host = asked.host.clone().filter(|host| !host.is_empty());
    // One call may save the profile the machine is made for, so there is no
    // moment between the host and its machine. A host that has a profile
    // keeps the one it has unless one is given, and one given replaces it.
    // Its lock is held from before the save, so two calls cannot save over
    // each other, and what the save replaced is kept to be put back.
    let mut saved = None;
    match (&host, asked.profile.take()) {
        (Some(host), Some(profile)) => {
            let (place, profile) =
                super::setup::checked_profile(&state, host, access.as_ref(), profile)?;
            let lock = rookery_core::provision::lock(&state.paths, host)
                .map_err(|error| Fail::new(code::CONFLICT, error.to_string()))?;
            let before = place.load(&state.paths).map_err(internal)?;
            place.save(&state.paths, &profile).map_err(internal)?;
            state.forget(&place.argument());
            state.refresh(false);
            saved = Some(Saved {
                place,
                before,
                lock: Some(lock),
            });
        }
        (Some(host), None) => {
            super::setup::saved_profile(&state, host)?;
        }
        (None, Some(_)) => {
            return Err(bad(
                "A profile is saved with a host's machine: name its host",
            ));
        }
        (None, None) => {}
    }
    let (paths, registries) = (state.paths.clone(), store(&state));
    let token = asked.token.clone();
    let host = host.unwrap_or_default();
    let held = saved.as_mut().and_then(|saved| saved.lock.take());
    let prepared = match blocking({
        let (paths, registries) = (paths.clone(), registries.clone());
        move || resource::prepare_held(&paths, &registries, &asked, held)
    })
    .await
    {
        Ok(prepared) => prepared,
        Err(fail) => {
            restore(&state, saved.as_ref());
            return Err(fail);
        }
    };
    let mut creating = match prepared {
        Ok(Asked::Again(kept)) => {
            let job = match state
                .runner
                .by_token(kept.subject(), "create", token.as_deref().unwrap_or(""))
                .await
            {
                Ok(Some(job)) => job,
                Ok(None) => {
                    restore(&state, saved.as_ref());
                    return Err(Fail::new(
                        code::CONFLICT,
                        format!(
                            "{} was asked for with this token, and its job is gone",
                            kept.record.id
                        ),
                    ));
                }
                Err(error) => {
                    restore(&state, saved.as_ref());
                    return Err(internal(error));
                }
            };
            let plugins = rookery_core::provision::list_plugins(&state.paths);
            return Ok(Json(Created {
                resource: described(&state, &kept, &plugins),
                job,
            }));
        }
        Ok(Asked::New(creating)) => creating,
        Err(said) => {
            // A profile this call saved does not stay for a machine that was
            // refused, and one it replaced is put back: the same call can be
            // made again.
            restore(&state, saved.as_ref());
            return Err(refusal(said));
        }
    };
    let kept = creating.kept().clone();
    let mut request = Request::new(kept.subject(), "create");
    request.host = kept.record.host.clone();
    request.token = token;
    request.held = creating.take_lock();
    request.secrets = creating.secrets();
    let before = reached(&paths, &host);
    let (watcher, runtime) = (state.clone(), tokio::runtime::Handle::current());
    let work = Work::Function(Box::new(move |output| {
        let paths = &watcher.paths;
        let done = creating.run(paths, &registries, &(watcher.provision)(), output);
        if let Err(error) = &done {
            let _ = writeln!(output, "✗ {error}");
        }
        // An address the provisioner gave is how the host is reached now.
        if !host.is_empty() && reached(paths, &host) != before {
            let _inside = runtime.enter();
            watcher.forget(&host);
            watcher.refresh(false);
        }
        done.is_ok()
    }));
    let job = match state.runner.start(request, work).await {
        Ok(job) => job,
        Err(error) => {
            restore(&state, saved.as_ref());
            return Err(refused(error));
        }
    };
    let plugins = rookery_core::provision::list_plugins(&state.paths);
    Ok(Json(Created {
        resource: described(&state, &kept, &plugins),
        job,
    }))
}

pub async fn actions(state: Arc<Serve>, id: String) -> Answer<Json<Vec<Action>>> {
    let kept = found(&state, &id)?;
    Ok(Json(resource::actions(&state.paths, &store(&state), &kept)))
}

/// Run an action of a resource that has been admitted: one that only reads
/// answers at once, the others are jobs of the resource.
pub(crate) async fn run_action(
    state: Arc<Serve>,
    id: String,
    action: String,
    given: Given,
    token: Option<String>,
) -> Answer<Acted> {
    let kept = found(&state, &id)?;
    // The same token again is the job it started, before anything is checked again.
    if let Some(token) = token.as_deref()
        && let Some(job) = state
            .runner
            .by_token(kept.subject(), &action, token)
            .await
            .map_err(internal)?
    {
        return Ok(Acted {
            job: Some(job),
            ..Acted::default()
        });
    }
    let (paths, registries, options) = (state.paths.clone(), store(&state), (state.provision)());
    let mut acting = blocking({
        let (paths, id, action) = (paths.clone(), id.clone(), action.clone());
        move || resource::prepare_action(&paths, &registries, &id, &action, &given)
    })
    .await?
    .map_err(refusal)?;
    if !acting.changes() {
        let answered = blocking(move || acting.run(&paths, &options, &mut std::io::sink()))
            .await?
            .map_err(|error| Fail::new(code::UNREACHABLE, error.message))?;
        return Ok(Acted {
            result: Some(answered.value()),
            ..Acted::default()
        });
    }
    let mut request = Request::new(kept.subject(), &action);
    request.role = resource::actions(&state.paths, &store(&state), &kept)
        .iter()
        .find(|known| known.name == action)
        .map_or(rookery_api::ops::Role::Admin, |known| known.role);
    request.host = kept.record.host.clone();
    request.token = token;
    request.held = acting.take_lock();
    request.secrets = acting.secrets();
    request.arguments = acting.shown();
    // A command leaves rookery's record as it is, so it holds no lock of the resource.
    let exec = action == resource::EXEC;
    request.changes = !exec;
    let ticket = match state.runner.reserve(request).await.map_err(refused)? {
        Reserved::Found(job) => {
            return Ok(Acted {
                job: Some(job),
                ..Acted::default()
            });
        }
        Reserved::New(ticket) => ticket,
    };
    // The job ends as the command did, or with 1 when it could not be run.
    let work = Work::Coded(Box::new(move |out| {
        match acting.run(&paths, &options, out) {
            // A command's output is its own, and its exit status says how it went.
            Ok(answered) if exec => answered.exit_code.unwrap_or(0),
            Ok(answered) => {
                say(out, &answered);
                answered.exit_code.unwrap_or(0)
            }
            Err(error) => {
                let _ = writeln!(out, "✗ {error}");
                1
            }
        }
    }));
    Ok(Acted {
        job: Some(ticket.run(work).await.map_err(internal)?),
        ..Acted::default()
    })
}

fn say(out: &mut dyn Write, answered: &Answered) {
    for line in answered.lines() {
        let _ = writeln!(out, "{line}");
    }
}

/// Check the caller against the action's own role and its availability.
fn admitted(state: &Serve, access: &Access, id: &str, action: &str, act: &Act) -> Answer<()> {
    let kept = found(state, id)?;
    let listed = resource::actions(&state.paths, &store(state), &kept);
    let Some(known) = listed.iter().find(|known| known.name == action) else {
        let names: Vec<&str> = listed.iter().map(|known| known.name.as_str()).collect();
        return Err(Fail::new(
            code::NOT_FOUND,
            format!(
                "{id} has no action {action}. Its actions: {}",
                names.join(", ")
            ),
        ));
    };
    admit(state, access, known, act)
}

pub async fn act(
    state: Arc<Serve>,
    access: Option<Access>,
    (id, action): (String, String),
    body: Bytes,
) -> Answer<Json<Acted>> {
    let act = parse(&body)?;
    admitted(&state, &caller(access), &id, &action, &act)?;
    let given = Given {
        arguments: act.arguments,
        ..Given::default()
    };
    Ok(answer(
        run_action(state, id, action, given, act.token).await?,
    ))
}

pub async fn change(
    state: Arc<Serve>,
    access: Option<Access>,
    id: String,
    body: Bytes,
) -> Answer<Json<Acted>> {
    let asked: ResourceChange = serde_json::from_slice(&body).map_err(bad)?;
    let act = Act::default();
    admitted(&state, &caller(access), &id, resource::CHANGE, &act)?;
    let given = Given {
        arguments: asked.arguments,
        spec: asked.spec,
        secrets: asked.secrets,
    };
    Ok(answer(
        run_action(state, id, resource::CHANGE.into(), given, asked.token).await?,
    ))
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct Token {
    /// Any text of the caller's; the same again answers with the first job.
    token: Option<String>,
}

impl Token {
    pub fn with(token: Option<String>) -> Token {
        Token { token }
    }
}

pub async fn remove(
    state: Arc<Serve>,
    access: Option<Access>,
    id: String,
    query: Token,
) -> Answer<Json<Acted>> {
    admitted(
        &state,
        &caller(access),
        &id,
        resource::REMOVE,
        &Act::default(),
    )?;
    Ok(answer(
        run_action(
            state,
            id,
            resource::REMOVE.into(),
            Given::default(),
            query.token,
        )
        .await?,
    ))
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct LogQuery {
    /// The last so many lines; all when 0 (default 100).
    tail: Option<u32>,
}

impl LogQuery {
    pub fn with(tail: Option<u32>) -> LogQuery {
        LogQuery { tail }
    }
}

/// How long a followed log goes on before it is ended.
const FOLLOW_FOR: Duration = Duration::from_secs(60 * 60);
/// How long the provisioner has to give the lines of a plain read.
const READ_FOR: Duration = Duration::from_secs(60);

/// A resource's log as it is now.
pub async fn logs(state: Arc<Serve>, id: String, query: LogQuery) -> Answer<Json<String>> {
    found(&state, &id)?;
    let tail = query.tail.unwrap_or(100);
    let (paths, registries) = (state.paths.clone(), store(&state));
    let text = blocking(move || {
        let mut out = vec![];
        resource::logs(&paths, &registries, &id, tail, false, READ_FOR, &mut out)?;
        Ok(out)
    })
    .await?
    .map_err(refusal)?;
    Ok(Json(String::from_utf8_lossy(&text).into_owned()))
}

/// Lines written to a channel, for a socket that sends them on.
struct Lines(tokio::sync::mpsc::UnboundedSender<Vec<u8>>);

impl Write for Lines {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0
            .send(data.to_vec())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A resource's log on a stream as it comes, then a JSON line with how it ended.
pub async fn follow(
    state: Arc<Serve>,
    id: String,
    tail: u32,
    socket: rookery_kernel::streams::Stream,
) {
    let (_, socket) = socket.split();
    let (sender, mut lines) = tokio::sync::mpsc::unbounded_channel();
    let (paths, registries) = (state.paths.clone(), store(&state));
    let reading = tokio::task::spawn_blocking(move || {
        resource::logs(
            &paths,
            &registries,
            &id,
            tail,
            true,
            FOLLOW_FOR,
            &mut Lines(sender),
        )
        .map_err(|error| error.to_string())
    });
    loop {
        tokio::select! {
            line = lines.recv() => match line {
                Some(data) => if socket.write(&data).await.is_err() { return },
                None => break,
            },
            _ = socket.closed() => return,
        }
    }
    let ended = match reading.await {
        Ok(Ok(())) => serde_json::json!({"exited": {"exit_code": 0}}),
        Ok(Err(error)) => serde_json::json!({"exited": {"exit_code": 1}, "error": error}),
        Err(error) => serde_json::json!({"exited": {"exit_code": 1}, "error": error.to_string()}),
    };
    let mut line = ended.to_string().into_bytes();
    line.push(b'\n');
    let _ = socket.write(&line).await;
    socket.eof();
    socket.close(None);
}
