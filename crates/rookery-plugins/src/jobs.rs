//! The jobs plugins declare (`contributes.jobs`), run by their host halves.
//!
//! Each declared job is a kind of job of the server's, `PLUGIN/NAME`, where `NAME` is its id without the
//! plugin's own prefix (`projects.import` of `projects` is `projects/import`). The plugin system adds a
//! handler for each kind to the server's jobs while the plugin is installed and valid. A job of the kind
//! starts the plugin's host half when it does not run, and runs the handler it registered with
//! `rookery.jobs.handle(id, ...)`, until `timeoutSecs` or the job's cancel.
//!
//! `enqueue` in the declaration says who may enqueue one besides an admin: `own` (the default), anyone
//! signed in and the plugin itself; `plugin`, the plugin alone; `admin`, admins alone. A job is recorded
//! with its caller as `enqueuedBy`.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use rookery_jobs::{JobHandler, JobRun};
use rookery_kernel::{Call, Caller, Kernel, RpcError};
use serde_json::{Value, json};

use crate::Plugins;
use crate::loaded::Loaded;

/// One job a plugin declares.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredJob {
    pub plugin: String,
    /// The id as the manifest has it, which the host half registers.
    pub id: String,
    /// `PLUGIN/NAME`.
    pub kind: String,
    pub enqueue: String,
    pub timeout: Duration,
    pub max_attempts: i32,
}

/// The jobs a valid plugin declares, but those an app service runs (`worker`), which have no host half
/// to run in.
pub fn declared(plugin: &Loaded) -> Vec<DeclaredJob> {
    if !plugin.valid() {
        return vec![];
    }
    let Some(manifest) = &plugin.manifest else {
        return vec![];
    };
    manifest
        .pointer("/contributes/jobs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|job| job.get("worker").is_none())
        .filter_map(|job| {
            let id = job.get("id")?.as_str()?.to_owned();
            let name = id
                .strip_prefix(&format!("{}.", plugin.id))
                .unwrap_or(&id)
                .to_owned();
            Some(DeclaredJob {
                plugin: plugin.id.clone(),
                kind: format!("{}/{name}", plugin.id),
                enqueue: job
                    .get("enqueue")
                    .and_then(Value::as_str)
                    .unwrap_or("own")
                    .to_owned(),
                timeout: Duration::from_secs(
                    job.get("timeoutSecs")
                        .and_then(Value::as_u64)
                        .unwrap_or(3600),
                ),
                max_attempts: job.get("maxAttempts").and_then(Value::as_i64).unwrap_or(3) as i32,
                id,
            })
        })
        .collect()
}

/// A kind's name, kept for the life of the server: a `JobHandler` names its kind as `&'static str`, and
/// there are only as many as plugins declare.
fn interned(kind: &str) -> &'static str {
    static KINDS: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    let mut kinds = KINDS.lock().unwrap();
    let kinds = kinds.get_or_insert_with(HashSet::new);
    if let Some(known) = kinds.get(kind) {
        return known;
    }
    let leaked: &'static str = Box::leak(kind.to_owned().into_boxed_str());
    kinds.insert(leaked);
    leaked
}

struct PluginJob {
    job: DeclaredJob,
    kind: &'static str,
    plugins: Weak<Plugins>,
}

impl JobHandler for PluginJob {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn max_attempts(&self) -> i32 {
        self.job.max_attempts
    }

    fn run(&self, run: JobRun) -> BoxFuture<'static, Result<Value, RpcError>> {
        let (job, plugins) = (self.job.clone(), self.plugins.clone());
        Box::pin(async move {
            let plugins = plugins
                .upgrade()
                .ok_or_else(|| RpcError::unavailable("the plugin system stopped"))?;
            let kernel = run.kernel().clone();
            // Not as the admin the server would be: a host half that failed starts again for an admin
            // alone, whoever enqueued the job.
            plugins.activate_as(&kernel, &job.plugin, false).await?;
            let mut call = Call::new(kernel, Caller::System(format!("job:{}", run.id)), None);
            call.deadline = Instant::now() + job.timeout;
            let params = json!({
                "params": run.params,
                "job": {
                    "id": run.id.to_string(),
                    "attempt": run.attempt,
                    "maxAttempts": job.max_attempts,
                    "kind": run.kind,
                    "subject": run.subject,
                },
            });
            let cancel = tokio_util::sync::CancellationToken::new();
            let watch = {
                let cancel = cancel.clone();
                async move {
                    run.cancelled().await;
                    cancel.cancel();
                }
            };
            let invoked = plugins.host.invoke_until(
                &call,
                &plugins,
                &job.plugin,
                "job",
                &job.id,
                params,
                Some(cancel),
            );
            let watcher = tokio::spawn(watch);
            let done = invoked.await;
            watcher.abort();
            done
        })
    }
}

/// Makes the server's jobs have a handler for each kind the plugins declare now, and no other of theirs.
pub fn sync(kernel: &Kernel, plugins: &Arc<Plugins>, before: &[DeclaredJob], now: &[DeclaredJob]) {
    for gone in before.iter().filter(|old| !now.contains(old)) {
        rookery_jobs::remove_handler(kernel, &gone.kind);
    }
    for job in now.iter().filter(|new| !before.contains(new)) {
        rookery_jobs::add_handler(
            kernel,
            &job.kind,
            Arc::new(PluginJob {
                kind: interned(&job.kind),
                job: job.clone(),
                plugins: Arc::downgrade(plugins),
            }),
        );
    }
}

/// Who besides an admin may enqueue a job of `kind`: `None` when no plugin declares it.
pub fn may_enqueue(
    jobs: &BTreeMap<String, DeclaredJob>,
    call: &Call,
    kind: &str,
) -> Option<Result<(), RpcError>> {
    let job = jobs.get(kind)?;
    let own = matches!(&call.caller, Caller::Plugin(id) if *id == job.plugin);
    let allowed = match job.enqueue.as_str() {
        "own" => own || matches!(call.caller, Caller::User { .. } | Caller::Local),
        "plugin" => own,
        _ => false,
    };
    Some(match allowed {
        true => Ok(()),
        false => Err(RpcError::forbidden(format!(
            "{kind} is enqueued by {}",
            match job.enqueue.as_str() {
                "plugin" => format!("the plugin {} alone", job.plugin),
                "own" => "people and its own plugin".to_owned(),
                _ => "admins".to_owned(),
            }
        ))),
    })
}
