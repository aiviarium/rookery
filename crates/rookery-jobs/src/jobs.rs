//! Jobs, and the handlers that run them inside the server.
//!
//! A [`JobHandler`] runs the jobs of one kind. The leader's worker claims a ready job, runs its
//! handler, renews the claim while it runs and records the outcome. The handler writes log items and
//! progress, and stops when `cancel_requested` is set. A failed attempt runs again, after a wait that
//! doubles, while attempts are left, unless the handler is not safe to repeat: its job has one attempt,
//! and one the server stopped in the middle of fails as interrupted for a person to decide.
//! `jobs.follow` streams the log with a cursor and ends with the outcome.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use rookery_kernel::streams::Initiator;
use rookery_kernel::{Call, Kernel, Level, Methods, RpcError};
use rookery_proto::ErrorBody;
use rookery_proto::jobs::{
    JobEnd, JobEnqueue, JobFollow, JobInfo, JobList, JobLog, JobProgress, JobQuery, JobRef,
    JobState, LogItem, LogItems,
};
use rookery_proto::stream::StreamOpened;
use rookery_store::jobs::{Filter, Job, NewJob, Outcome};
use serde_json::Value;
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::time;

/// Runs the jobs of one kind inside the server.
pub trait JobHandler: Send + Sync + 'static {
    /// The kind it runs, such as `hosts.apply`.
    fn kind(&self) -> &'static str;

    /// Whether an attempt may run again after it failed or the server stopped during it. One that is
    /// not, such as an apply, gets one attempt.
    fn repeatable(&self) -> bool {
        true
    }

    /// Attempts of a job of a kind that may repeat.
    fn max_attempts(&self) -> i32 {
        3
    }

    fn run(&self, job: JobRun) -> BoxFuture<'static, Result<Value, RpcError>>;
}

/// How the worker runs.
#[derive(Debug, Clone)]
pub struct Options {
    /// Jobs run at once.
    pub concurrency: usize,
    /// The wait before a failed attempt runs again, doubled for each attempt.
    pub backoff: Duration,
    /// How long a claim lasts; the worker renews it at a third of that.
    pub lease: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            concurrency: 8,
            backoff: Duration::from_secs(5),
            lease: Duration::from_secs(30),
        }
    }
}

/// One attempt of a job, as its handler sees it.
#[derive(Clone)]
pub struct JobRun {
    kernel: Kernel,
    pub id: Uuid,
    pub kind: String,
    pub params: Value,
    pub attempt: i32,
    pub subject: Option<String>,
    cancel: CancellationToken,
    worker: String,
}

impl JobRun {
    /// Writes a line to the job's log.
    pub async fn log(&self, level: &str, text: &str, data: Option<Value>) -> Result<i64, RpcError> {
        let seq = self
            .kernel
            .store
            .jobs()
            .log_claimed(self.id, &self.worker, level, text, data.as_ref())
            .await?;
        signals(&self.kernel).changed(self.id);
        Ok(seq)
    }

    pub async fn info(&self, text: &str) -> Result<i64, RpcError> {
        self.log("info", text, None).await
    }

    /// Says how far the job is.
    pub async fn progress(&self, progress: Value) -> Result<(), RpcError> {
        self.kernel
            .store
            .jobs()
            .set_claimed_progress(self.id, &self.worker, &progress)
            .await?;
        signals(&self.kernel).changed(self.id);
        Ok(())
    }

    /// Resolves when someone asked the job to stop. The handler ends the attempt, and the job is
    /// cancelled whatever the handler returns.
    pub async fn cancelled(&self) {
        self.cancel.cancelled().await
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn kernel(&self) -> &Kernel {
        &self.kernel
    }
}

/// Wakes the worker and the followers of jobs.
pub struct Signals {
    wake: Notify,
    changed: broadcast::Sender<Uuid>,
    running: Mutex<HashMap<Uuid, CancellationToken>>,
}

impl Default for Signals {
    fn default() -> Self {
        Signals {
            wake: Notify::new(),
            changed: broadcast::channel(1024).0,
            running: Mutex::default(),
        }
    }
}

impl Signals {
    fn changed(&self, id: Uuid) {
        let _ = self.changed.send(id);
    }
}

fn signals(kernel: &Kernel) -> Arc<Signals> {
    kernel.facility::<Signals>()
}

struct Registry {
    handlers: HashMap<&'static str, Arc<dyn JobHandler>>,
    options: Options,
}

fn registry(kernel: &Kernel) -> Result<Arc<Registry>, RpcError> {
    kernel
        .get_facility::<Registry>()
        .ok_or_else(|| RpcError::internal("jobs are not set up"))
}

fn state(job: &Job) -> JobState {
    match job.state.as_str() {
        "queued" => JobState::Queued,
        "running" => JobState::Running,
        "succeeded" => JobState::Succeeded,
        "failed" => JobState::Failed,
        _ => JobState::Cancelled,
    }
}

fn error_body(value: &Option<Value>) -> Option<ErrorBody> {
    value
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

pub fn info(job: Job) -> JobInfo {
    JobInfo {
        id: job.id.to_string(),
        state: state(&job),
        error: error_body(&job.error),
        kind: job.kind,
        params: job.params,
        subject: job.subject,
        attempts: job.attempts,
        max_attempts: job.max_attempts,
        cancel_requested: job.cancel_requested,
        progress: job.progress,
        result: job.result,
        enqueued_by: job.enqueued_by,
        created_at: time(job.created_at),
        started_at: job.started_at.map(time),
        finished_at: job.finished_at.map(time),
    }
}

fn item(i: rookery_store::jobs::LogItem) -> LogItem {
    LogItem {
        seq: i.seq,
        at: time(i.at),
        attempt: i.attempt,
        level: i.level,
        text: i.text,
        data: i.data,
    }
}

/// Enqueues a job of a kind a handler runs, for code inside the server as for `jobs.enqueue`.
pub async fn enqueue(
    kernel: &Kernel,
    kind: &str,
    params: Value,
    subject: Option<&str>,
    by: &str,
) -> Result<Job, RpcError> {
    let registry = registry(kernel)?;
    let handler = registry
        .handlers
        .get(kind)
        .ok_or_else(|| RpcError::invalid(format!("no handler runs jobs of the kind {kind}")))?;
    if let Some(subject) = subject {
        rookery_store::access::check_resource(subject)?;
    }
    let max_attempts = if handler.repeatable() {
        handler.max_attempts().max(1)
    } else {
        1
    };
    let job = kernel
        .store
        .jobs()
        .enqueue(NewJob {
            kind,
            params: &params,
            max_attempts,
            run_after: None,
            subject,
            enqueued_by: by,
        })
        .await?;
    signals(kernel).wake.notify_one();
    Ok(job)
}

/// The job, if the caller may see it: the one who enqueued it, or an admin.
async fn visible(call: &Call, id: &str) -> Result<Job, RpcError> {
    let id = Uuid::parse_str(id)
        .map_err(|_| RpcError::invalid(format!("{id:?} is not the id of a job")))?;
    match call.store().jobs().get(id).await? {
        Some(job) if call.is_admin() || job.enqueued_by == call.audit_name() => Ok(job),
        _ => Err(RpcError::not_found("no such job")),
    }
}

/// A job of the calling plugin, for its worker.
async fn plugins_job(call: &Call, plugin: Option<&str>, id: &str) -> Result<Job, RpcError> {
    let plugin = call.plugin(plugin)?;
    let id = Uuid::parse_str(id)
        .map_err(|_| RpcError::invalid(format!("{id:?} is not the id of a job")))?;
    match call.store().jobs().get(id).await? {
        Some(job)
            if job
                .kind
                .rsplit_once('/')
                .is_some_and(|(owner, _)| owner == plugin) =>
        {
            Ok(job)
        }
        _ => Err(RpcError::not_found("no such job of the plugin")),
    }
}

pub fn register(methods: &mut Methods, handlers: Vec<Arc<dyn JobHandler>>, options: Options) {
    let mut map = HashMap::new();
    for handler in handlers {
        assert!(
            map.insert(handler.kind(), handler.clone()).is_none(),
            "two handlers for jobs of the kind {}",
            handler.kind()
        );
    }
    let registry = Arc::new(Registry {
        handlers: map,
        options,
    });
    methods.on_started(move |kernel| {
        kernel.set_facility(Registry {
            handlers: registry.handlers.clone(),
            options: registry.options.clone(),
        });
        if !registry.handlers.is_empty() {
            tokio::spawn(work(kernel.clone()));
        }
    });
    methods
        .add("jobs.enqueue", |call: Call, p: JobEnqueue| async move {
            let job = enqueue(
                &call.kernel,
                &p.kind,
                p.params,
                p.subject.as_deref(),
                &call.audit_name(),
            )
            .await?;
            Ok(info(job))
        })
        .summary("Enqueues a job of a kind a handler in the server runs.")
        .level(Level::Admin);
    methods
        .add("jobs.get", |call: Call, p: JobRef| async move { Ok(info(visible(&call, &p.id).await?)) })
        .summary("A job: its state, attempts, progress and outcome. The one who enqueued it, or an admin.")
        .level(Level::Own);
    methods
        .add("jobs.list", |call: Call, p: JobQuery| async move {
            let me = call.audit_name();
            let state = p.state.map(|s| {
                serde_json::to_value(s)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default()
            });
            let before = p
                .before
                .as_deref()
                .map(Uuid::parse_str)
                .transpose()
                .map_err(|_| RpcError::invalid("before is the id of a job"))?;
            let filter = Filter {
                enqueued_by: (!call.is_admin()).then_some(me.as_str()),
                state: state.as_deref(),
                kind: p.kind.as_deref(),
                subject: p.subject.as_deref(),
                before,
                limit: p.limit.unwrap_or(50),
            };
            Ok(JobList {
                jobs: call
                    .store()
                    .jobs()
                    .list(&filter)
                    .await?
                    .into_iter()
                    .map(info)
                    .collect(),
            })
        })
        .summary("Jobs, newest first: all for an admin, one's own for others.")
        .level(Level::Own);
    methods
        .add("jobs.output", |call: Call, p: JobFollow| async move {
            let job = visible(&call, &p.id).await?;
            let items = call.store().jobs().log_after(job.id, p.after, 1000).await?;
            Ok(LogItems {
                items: items.into_iter().map(item).collect(),
                state: state(&job),
            })
        })
        .summary("A job's log after a cursor, up to 1000 items.")
        .level(Level::Own);
    methods
        .add("jobs.follow", |call: Call, p: JobFollow| async move {
            let job = visible(&call, &p.id).await?;
            let stream = call.conn()?.streams().open(Initiator::Peer)?;
            let id = stream.id();
            tokio::spawn(follow(call.kernel.clone(), job.id, p.after, stream));
            Ok(StreamOpened { stream: id, offset: Some(p.after.max(0) as u64) })
        })
        .summary("A job's log as a stream from a cursor: a JSON line for each item, and a last line with the outcome.")
        .stream()
        .level(Level::Own);
    methods
        .add("jobs.cancel", |call: Call, p: JobRef| async move {
            let job = visible(&call, &p.id).await?;
            let job = call.store().jobs().cancel(job.id).await?;
            let signals = signals(&call.kernel);
            if let Some(token) = signals.running.lock().unwrap().get(&job.id) {
                token.cancel();
            }
            signals.changed(job.id);
            call.audit(
                "job.cancel",
                job.subject.as_deref(),
                serde_json::json!({ "job": job.id, "kind": job.kind }),
            )
            .await;
            Ok(info(job))
        })
        .summary(
            "Cancels a job: a queued one at once, a running one by asking its handler to stop.",
        )
        .level(Level::Own);
    methods
        .add("jobs.retry", |call: Call, p: JobRef| async move {
            let job = visible(&call, &p.id).await?;
            let job = call.store().jobs().retry(job.id).await?;
            signals(&call.kernel).wake.notify_one();
            call.audit(
                "job.retry",
                job.subject.as_deref(),
                serde_json::json!({ "job": job.id, "kind": job.kind }),
            )
            .await;
            Ok(info(job))
        })
        .summary("Runs a failed or cancelled job again, with one more attempt.")
        .level(Level::Own);
    methods
        .add("jobs.progress", |call: Call, p: JobProgress| async move {
            let job = plugins_job(&call, p.plugin.as_deref(), &p.id).await?;
            call.store()
                .jobs()
                .set_progress(job.id, &p.progress)
                .await?;
            signals(&call.kernel).changed(job.id);
            Ok(rookery_kernel::Empty {})
        })
        .summary("A plugin's worker says how far a job of its plugin is.")
        .level(Level::Plugin);
    methods
        .add("jobs.log", |call: Call, p: JobLog| async move {
            if !matches!(p.level.as_str(), "debug" | "info" | "warn" | "error") {
                return Err(RpcError::invalid("a level is debug, info, warn or error"));
            }
            let job = plugins_job(&call, p.plugin.as_deref(), &p.id).await?;
            let seq = call
                .store()
                .jobs()
                .log(job.id, &p.level, &p.text, p.data.as_ref())
                .await?;
            signals(&call.kernel).changed(job.id);
            Ok(rookery_proto::jobs::Published { seq })
        })
        .summary("A plugin's worker writes a line to the log of a job of its plugin.")
        .level(Level::Plugin);
}

/// Streams a job's log from `after`, then its outcome.
async fn follow(kernel: Kernel, id: Uuid, mut after: i64, stream: rookery_kernel::streams::Stream) {
    let (_reader, writer) = stream.split();
    let mut changes = signals(&kernel).changed.subscribe();
    let jobs = kernel.store.jobs();
    loop {
        // Read the state before the items, so that the items of a job that ended are all sent.
        let job = match jobs.get(id).await {
            Ok(Some(job)) => job,
            _ => break,
        };
        let items = match jobs.log_after(id, after, 1000).await {
            Ok(items) => items,
            Err(_) => break,
        };
        let more = items.len() == 1000;
        for i in items {
            after = i.seq;
            let mut line = serde_json::to_vec(&item(i)).expect("a log item serializes");
            line.push(b'\n');
            if writer.write(&line).await.is_err() {
                return;
            }
        }
        if more {
            continue;
        }
        if job.is_done() {
            let end = JobEnd {
                state: state(&job),
                error: error_body(&job.error),
                result: job.result,
            };
            let mut line = serde_json::to_vec(&end).expect("an end serializes");
            line.push(b'\n');
            if writer.write(&line).await.is_ok() {
                writer.eof();
            }
            writer.close(None);
            return;
        }
        // Wait for this job to change, or for the follower to go.
        loop {
            tokio::select! {
                changed = changes.recv() => match changed {
                    Ok(changed) if changed != id => continue,
                    // Behind: read again from the store.
                    _ => break,
                },
                _ = writer.closed() => return,
                _ = kernel.shutdown.cancelled() => {
                    writer.close(Some(RpcError::unavailable("the server stops").body()));
                    return;
                }
            }
        }
    }
    writer.close(Some(RpcError::not_found("the job is gone").body()));
}

/// The leader's worker: recovers what a stopped server left running, then claims and runs jobs.
async fn work(kernel: Kernel) {
    let Ok(registry) = registry(&kernel) else {
        return;
    };
    let worker = format!("server:{}", Uuid::now_v7());
    let kinds: Vec<String> = registry.handlers.keys().map(|k| k.to_string()).collect();
    let once: Vec<String> = registry
        .handlers
        .values()
        .filter(|h| !h.repeatable())
        .map(|h| h.kind().to_string())
        .collect();
    match kernel.store.jobs().recover(&kinds, &once, &worker).await {
        Ok(recovered) => {
            for job in recovered {
                tracing::warn!(job = %job.id, kind = %job.kind, state = %job.state, "a job the server stopped in was ended or queued again");
                signals(&kernel).changed(job.id);
            }
        }
        Err(e) => tracing::error!(error = %e, "recovering jobs failed"),
    }
    let slots = Arc::new(tokio::sync::Semaphore::new(
        registry.options.concurrency.max(1),
    ));
    let signals = signals(&kernel);
    loop {
        let Ok(slot) = slots.clone().acquire_owned().await else {
            return;
        };
        // Registered before the store is asked, so that a job enqueued meanwhile wakes it.
        let woken = signals.wake.notified();
        tokio::pin!(woken);
        woken.as_mut().enable();
        let claim = format!("{worker}:{}", Uuid::now_v7());
        match kernel
            .store
            .jobs()
            .claim(&kinds, &claim, registry.options.lease)
            .await
        {
            Ok(Some(job)) => {
                tokio::spawn(attempt(kernel.clone(), registry.clone(), claim, job, slot));
                continue;
            }
            Ok(None) => drop(slot),
            Err(e) => {
                drop(slot);
                tracing::warn!(error = %e, "claiming a job failed");
            }
        }
        let wait = match kernel.store.jobs().next_ready(&kinds).await {
            Ok(Some(at)) => (at - chrono::Utc::now())
                .to_std()
                .unwrap_or(Duration::ZERO)
                .min(Duration::from_secs(30)),
            _ => Duration::from_secs(30),
        };
        tokio::select! {
            _ = &mut woken => {}
            _ = tokio::time::sleep(wait) => {}
            _ = kernel.shutdown.cancelled() => return,
        }
    }
}

async fn attempt(
    kernel: Kernel,
    registry: Arc<Registry>,
    worker: String,
    job: Job,
    _slot: tokio::sync::OwnedSemaphorePermit,
) {
    let Some(handler) = registry.handlers.get(job.kind.as_str()).cloned() else {
        return;
    };
    let cancel = CancellationToken::new();
    if job.cancel_requested {
        cancel.cancel();
    }
    let signals = signals(&kernel);
    signals
        .running
        .lock()
        .unwrap()
        .insert(job.id, cancel.clone());
    signals.changed(job.id);
    let run = JobRun {
        kernel: kernel.clone(),
        id: job.id,
        kind: job.kind.clone(),
        params: job.params.clone(),
        attempt: job.attempts,
        subject: job.subject.clone(),
        cancel: cancel.clone(),
        worker: worker.clone(),
    };
    let lease = registry.options.lease;
    let mut beat = tokio::time::interval(lease / 3);
    beat.tick().await;
    let running = handler.run(run);
    tokio::pin!(running);
    let done = loop {
        tokio::select! {
            done = &mut running => break Some(done),
            _ = beat.tick() => match kernel.store.jobs().heartbeat(job.id, &worker, lease).await {
                Ok(true) => cancel.cancel(),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(job = %job.id, error = %e, "a job's claim was lost; its attempt stops");
                    break None;
                }
            },
            // The server stops: the attempt is left running, for the next leader to recover.
            _ = kernel.shutdown.cancelled() => break None,
        }
    };
    signals.running.lock().unwrap().remove(&job.id);
    let Some(done) = done else { return };
    let outcome = match done {
        _ if cancel.is_cancelled() => Outcome::Cancelled,
        Ok(result) => Outcome::Succeeded(result),
        Err(e) => {
            let retry_in = handler.repeatable().then(|| {
                registry
                    .options
                    .backoff
                    .saturating_mul(2u32.saturating_pow((job.attempts - 1).max(0) as u32))
            });
            Outcome::Failed {
                error: serde_json::to_value(e.body()).expect("an error serializes"),
                retry_in,
            }
        }
    };
    match kernel.store.jobs().finish(job.id, &worker, outcome).await {
        Ok(finished) => {
            if finished.state == "queued" {
                signals.wake.notify_one();
            }
        }
        Err(e) => tracing::warn!(job = %job.id, error = %e, "recording a job's outcome failed"),
    }
    signals.changed(job.id);
}
