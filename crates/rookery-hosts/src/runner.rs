//! Rookery's actions as jobs of the server. A method prepares the work and takes the subject's lock,
//! a lease, so that a refusal comes before there is a job; the job is then enqueued with the action
//! in its params, and its handler runs the work on a thread of its own. The output goes into the job's
//! log a line at a time, with the secrets of the request masked. Cancelling the job stops its
//! programs, or ends its session on the host.
//!
//! Apply, provisioning, roll-outs and the server's own actions get one attempt: a person decides
//! whether to run one again. A host's action that runs as a session there is a job of the kind
//! `hosts.session`, which keeps the session's id and how far its output was copied in its row, so that
//! a server that takes over follows it on.

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use rookery_core::paths::Paths;
use rookery_core::runner::{self as core, Controller, Lock, Output, Request, Stop, Subject, Work};
use rookery_jobs::{JobHandler, JobRun};
use rookery_kernel::{Kernel, RpcError};
use rookery_proto::jobs::JobInfo;
use serde_json::{Value, json};
use uuid::Uuid;

thread_local! {
    /// What the work of the job this thread runs leaves in the job's result besides its exit code.
    static RESULT: std::cell::RefCell<serde_json::Map<String, Value>> = Default::default();
}

/// Put `value` in the result of the job this thread runs under `key`, such as a roll-out's report.
pub fn set_result(key: &str, value: Value) {
    RESULT.with(|result| {
        result.borrow_mut().insert(key.to_owned(), value);
    });
}

tokio::task_local! {
    /// Who a method is called by, as the audit log names them: the enqueuer of the jobs it starts.
    pub static CALLER: String;
}

/// The kinds of jobs the runner's handlers run.
pub const HOST_ACTION: &str = "hosts.act";
pub const HOST_SESSION: &str = "hosts.session";
pub const PROVISIONER_ACTION: &str = "provisioners.act";
pub const RESOURCE_ACTION: &str = "resources.act";
pub const INSTALLATION_ACTION: &str = "installation.act";

/// The resource a job is about: `env://NAME` for a host, `provisioner://ID`, `resource://ID`, and
/// `rookery://installation`.
pub fn subject_uri(paths: &Paths, subject: &Subject) -> String {
    match subject.by_id(paths) {
        subject @ Subject::Host(_) => match subject.named(paths) {
            Subject::Host(name) => crate::uri(&name),
            _ => unreachable!("a host stays a host"),
        },
        Subject::Provisioner(id) => format!("provisioner://{id}"),
        Subject::Resource(id) => format!("resource://{id}"),
        Subject::Installation => "rookery://installation".into(),
    }
}

fn kind_of(subject: &Subject, work: &Work) -> &'static str {
    match (subject, work) {
        (Subject::Host(_), Work::Session { .. }) => HOST_SESSION,
        (Subject::Host(_), _) => HOST_ACTION,
        (Subject::Provisioner(_), _) => PROVISIONER_ACTION,
        (Subject::Resource(_), _) => RESOURCE_ACTION,
        (Subject::Installation, _) => INSTALLATION_ACTION,
    }
}

/// Finds the controller of a host by name, for a session a job follows on after a takeover.
pub type Resume = Arc<dyn Fn(&str) -> Option<Box<dyn Controller>> + Send + Sync>;

struct Pending {
    work: Work,
    lock: Option<Lock>,
    hidden: Vec<String>,
}

struct Inner {
    /// Given again each time the server leads, with the kernel of that time.
    paths: std::sync::RwLock<Option<Paths>>,
    kernel: std::sync::RwLock<Option<Kernel>>,
    /// The work of jobs enqueued and not yet claimed, by the ticket in their params.
    pending: Mutex<HashMap<Uuid, Pending>>,
    resume: Mutex<Option<Resume>>,
}

/// Starts jobs. Cheap to clone.
#[derive(Clone)]
pub struct Runner(Arc<Inner>);

/// What reserving a job came to.
pub enum Reserved {
    /// The job a call before with the same token started.
    Found(JobInfo),
    /// This call's job, to run or to fail.
    New(Ticket),
}

/// A job whose subject's lock is held, before its work is enqueued.
pub struct Ticket {
    runner: Runner,
    subject: Subject,
    uri: String,
    action: String,
    host: String,
    arguments: Vec<String>,
    token: Option<String>,
    lock: Option<Lock>,
    hidden: Vec<String>,
}

fn by() -> String {
    CALLER
        .try_with(String::clone)
        .unwrap_or_else(|_| "system:hosts".into())
}

impl Runner {
    /// A runner whose handlers can be registered before the server leads; [`Runner::attach`] gives it
    /// the rest.
    pub fn detached() -> Runner {
        Runner(Arc::new(Inner {
            paths: Default::default(),
            kernel: Default::default(),
            pending: Mutex::default(),
            resume: Mutex::default(),
        }))
    }

    pub fn attach(&self, paths: Paths, kernel: Kernel) {
        *self.0.paths.write().unwrap() = Some(paths);
        *self.0.kernel.write().unwrap() = Some(kernel);
        // Work prepared under a kernel before belongs to a server that led then.
        self.0.pending.lock().unwrap().clear();
    }

    fn paths(&self) -> Paths {
        self.0
            .paths
            .read()
            .unwrap()
            .clone()
            .expect("the runner is attached before a job starts")
    }

    pub fn set_resume(&self, resume: Resume) {
        *self.0.resume.lock().unwrap() = Some(resume);
    }

    pub fn kernel(&self) -> Kernel {
        self.0
            .kernel
            .read()
            .unwrap()
            .clone()
            .expect("the runner is attached before a job starts")
    }

    /// The handlers of the kinds of jobs the runner starts.
    pub fn handlers(&self) -> Vec<Arc<dyn JobHandler>> {
        [
            HOST_ACTION,
            HOST_SESSION,
            PROVISIONER_ACTION,
            RESOURCE_ACTION,
            INSTALLATION_ACTION,
        ]
        .into_iter()
        .map(|kind| {
            Arc::new(Handler {
                kind,
                runner: self.clone(),
            }) as Arc<dyn JobHandler>
        })
        .collect()
    }

    /// Start the work as a job. A token used before for the same subject and action answers with that
    /// job, and nothing is started. A refusal, such as a lock that is taken, comes before there is a
    /// job.
    pub async fn start(&self, request: Request, work: Work) -> Result<JobInfo, String> {
        match self.reserve(request).await? {
            Reserved::Found(job) => Ok(job),
            Reserved::New(ticket) => ticket.run(work).await,
        }
    }

    /// Take the subject's lock, or find the job a call before with the same token started.
    pub async fn reserve(&self, mut request: Request) -> Result<Reserved, String> {
        let paths = self.paths();
        let kernel = self.kernel();
        let held = request.held.take();
        // The store and the leases are asked from a thread that may wait.
        let prepared = tokio::task::spawn_blocking(move || {
            let subject = request.subject.by_id(&paths);
            let uri = subject_uri(&paths, &subject);
            let host = core::hosts_named(&paths, &core::hosts_by_id(&paths, &request.host));
            (request, subject, uri, host)
        })
        .await
        .map_err(|e| e.to_string())?;
        let (request, subject, uri, host) = prepared;
        if let Some(token) = &request.token {
            for kind in kind_candidates(&subject) {
                if let Some(job) = kernel
                    .store
                    .jobs()
                    .by_token(kind, Some(&uri), Some(&request.action), token)
                    .await
                    .map_err(|e| e.to_string())?
                {
                    return Ok(Reserved::Found(rookery_jobs::jobs::info(job)));
                }
            }
        }
        let lock = match (held, request.changes) {
            (Some(held), _) => Some(held),
            (None, true) => {
                let (paths, subject, action) =
                    (self.paths(), subject.clone(), request.action.clone());
                Some(
                    tokio::task::spawn_blocking(move || {
                        core::lock(&paths, &subject, &action).map_err(|e| e.to_string())
                    })
                    .await
                    .map_err(|e| e.to_string())??,
                )
            }
            (None, false) => None,
        };
        let hidden = core::hidden(request.secrets.iter().map(String::as_str));
        let arguments = request
            .arguments
            .iter()
            .filter(|argument| {
                !request
                    .secrets
                    .iter()
                    .any(|secret| !secret.is_empty() && argument.contains(secret.as_str()))
            })
            .cloned()
            .collect();
        Ok(Reserved::New(Ticket {
            runner: self.clone(),
            subject,
            uri,
            action: request.action,
            host,
            arguments,
            token: request.token,
            lock,
            hidden,
        }))
    }
}

fn kind_candidates(subject: &Subject) -> &'static [&'static str] {
    match subject {
        Subject::Host(_) => &[HOST_ACTION, HOST_SESSION],
        Subject::Provisioner(_) => &[PROVISIONER_ACTION],
        Subject::Resource(_) => &[RESOURCE_ACTION],
        Subject::Installation => &[INSTALLATION_ACTION],
    }
}

impl Ticket {
    /// Enqueue the job with its work: the job as it was enqueued.
    pub async fn run(self, work: Work) -> Result<JobInfo, String> {
        let Ticket {
            runner,
            subject,
            uri,
            action,
            host,
            arguments,
            token,
            lock,
            hidden,
        } = self;
        let kind = kind_of(&subject, &work);
        let session = match &work {
            Work::Session { id, .. } => Some(id.clone()),
            _ => None,
        };
        let ticket = Uuid::now_v7();
        let mut params = json!({
            "action": action,
            "subject": subject.key(),
            "host": host,
            "arguments": arguments,
            "ticket": ticket.to_string(),
        });
        if let Some(session) = &session {
            params["session"] = json!(session);
        }
        runner
            .0
            .pending
            .lock()
            .unwrap()
            .insert(ticket, Pending { work, lock, hidden });
        let kernel = runner.kernel();
        let made = rookery_jobs::jobs::enqueue_with_token(
            &kernel,
            kind,
            params,
            Some(&uri),
            &by(),
            token.as_deref(),
        )
        .await;
        let job = match made {
            Ok(job) => job,
            Err(e) => {
                runner.0.pending.lock().unwrap().remove(&ticket);
                return Err(e.to_string());
            }
        };
        // Another call with the same token was first: this work does not run.
        if job.params["ticket"].as_str() != Some(ticket.to_string().as_str()) {
            let pending = runner.0.pending.lock().unwrap().remove(&ticket);
            drop_off_runtime(pending);
        } else if let Some(session) = &session {
            let _ = kernel.store.jobs().set_session(job.id, session, 0).await;
        }
        Ok(rookery_jobs::jobs::info(job))
    }

    /// End the job as failed without running anything, with the reason in its log: the job.
    pub async fn fail(self, reason: &str) -> Result<JobInfo, String> {
        let reason = reason.to_owned();
        self.run(Work::Function(Box::new(move |out: &mut Output| {
            let _ = writeln!(out, "✗ {reason}");
            false
        })))
        .await
    }
}

/// Drops what may wait on the database, such as a lock, off the runtime's threads.
fn drop_off_runtime<T: Send + 'static>(value: T) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn_blocking(move || drop(value));
        }
        Err(_) => drop(value),
    }
}

struct Handler {
    kind: &'static str,
    runner: Runner,
}

impl JobHandler for Handler {
    fn kind(&self) -> &'static str {
        self.kind
    }

    /// Only a session on a host is followed on by the next server: anything else ran once.
    fn repeatable(&self) -> bool {
        self.kind == HOST_SESSION
    }

    fn max_attempts(&self) -> i32 {
        5
    }

    /// A session that ended badly ended; only a takeover follows it again.
    fn retries(&self, _: &RpcError) -> bool {
        false
    }

    fn run(&self, job: JobRun) -> BoxFuture<'static, Result<Value, RpcError>> {
        Box::pin(run(self.runner.clone(), job))
    }
}

/// How long a claimed job waits for its work, which the method that enqueued it puts in place.
const PREPARED: Duration = Duration::from_secs(10);

async fn run(runner: Runner, job: JobRun) -> Result<Value, RpcError> {
    let ticket = job.params["ticket"]
        .as_str()
        .and_then(|ticket| Uuid::parse_str(ticket).ok());
    let started = Instant::now();
    let pending = loop {
        let found = ticket.and_then(|ticket| runner.0.pending.lock().unwrap().remove(&ticket));
        if found.is_some() || started.elapsed() > PREPARED {
            break found;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let (work, lock, hidden, since) = match pending {
        Some(Pending { work, lock, hidden }) => (work, lock, hidden, 0),
        None => {
            // A session on a host, which a server before this one started: follow it on.
            let resumed = match (
                job.kind.as_str(),
                runner.kernel().store.jobs().session(job.id).await,
            ) {
                (HOST_SESSION, Ok(Some((session, offset)))) => {
                    let resume = runner.0.resume.lock().unwrap().clone();
                    let host = job.params["host"].as_str().unwrap_or_default().to_owned();
                    resume.and_then(|resume| resume(&host)).map(|controller| {
                        (
                            Work::Session {
                                controller,
                                id: session,
                            },
                            offset as u64,
                        )
                    })
                }
                _ => None,
            };
            match resumed {
                Some((work, since)) => (work, None, vec![], since),
                None => {
                    return Err(RpcError::unavailable(
                        "the server that prepared this job stopped before it ran: start the action again",
                    ));
                }
            }
        }
    };
    let stop = Arc::new(Stop::default());
    let (lines, mut taken) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let handle = tokio::runtime::Handle::current();
    let (kernel, id) = (runner.kernel(), job.id);
    let worker = {
        let stop = stop.clone();
        tokio::task::spawn_blocking(move || {
            let code = core::with_stop(stop.clone(), || {
                let mut sink = Sink(lines);
                let mut out = Output::new(&mut sink, hidden);
                let record = |offset: u64, session: &str| {
                    let session = session.to_owned();
                    let _ = handle.block_on(kernel.store.jobs().set_session(
                        id,
                        &session,
                        offset as i64,
                    ));
                };
                let code = execute(work, &mut out, since, &record);
                if let Some(by) = stop.asked() {
                    let _ = writeln!(out, "✗ Stopped by {by}");
                }
                code
            });
            drop(lock);
            (
                code,
                RESULT.with(|result| std::mem::take(&mut *result.borrow_mut())),
            )
        })
    };
    let mut pending = Vec::new();
    let mut asked = false;
    loop {
        tokio::select! {
            chunk = tokio::time::timeout(Duration::from_millis(500), taken.recv()) => match chunk {
                Ok(Some(chunk)) => {
                    pending.extend_from_slice(&chunk);
                    while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = pending.drain(..=end).collect();
                        write_line(&job, &line).await;
                    }
                }
                Ok(None) => break,
                // A line that waits for its end is written as it is, such as a prompt.
                Err(_) => if !pending.is_empty() {
                    let line = std::mem::take(&mut pending);
                    write_line(&job, &line).await;
                },
            },
            _ = job.cancelled(), if !asked => {
                asked = true;
                stop.ask("a request to cancel the job");
            }
        }
    }
    if !pending.is_empty() {
        write_line(&job, &pending).await;
    }
    let (code, mut result) = worker
        .await
        .map_err(|e| RpcError::internal(format!("the job's work ended: {e}")))?;
    result.insert("exit".into(), json!(code));
    match code {
        Some(0) => Ok(Value::Object(result)),
        Some(code) => Err(RpcError::internal(format!(
            "{} ended with exit code {code}",
            job.params["action"].as_str().unwrap_or("it")
        ))
        .with_details(Value::Object(result))),
        None => Err(RpcError::internal(format!(
            "{} did not end with an exit code",
            job.params["action"].as_str().unwrap_or("it")
        ))),
    }
}

async fn write_line(job: &JobRun, line: &[u8]) {
    let text = String::from_utf8_lossy(line);
    let text = text.trim_end_matches(['\n', '\r']);
    let level = match text.starts_with('✗') {
        true => "error",
        false => "info",
    };
    if let Err(e) = job.log(level, text, None).await {
        tracing::warn!(job = %job.id, error = %e, "writing a job's log failed");
    }
}

/// The job's output, on its way to the log.
struct Sink(tokio::sync::mpsc::UnboundedSender<Vec<u8>>);

impl Write for Sink {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.send(data.to_vec());
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run the work to its end, its output to `out`: the exit code.
fn execute(work: Work, out: &mut Output, since: u64, record: &dyn Fn(u64, &str)) -> Option<i32> {
    match work {
        Work::Function(function) => Some(if function(out) { 0 } else { 1 }),
        Work::Coded(function) => Some(function(out)),
        Work::Program(program) => core::run_program(*program, out),
        Work::Session { controller, id } => follow(controller, &id, out, since, record),
    }
}

/// How long a session's output is asked for again while there is no news,
/// and how long a host may not answer before its job gives up on it.
const POLL: Duration = Duration::from_millis(500);
const PATIENCE: Duration = Duration::from_secs(300);

/// Copy a session's output from `since` as it comes, until it has ended: its exit code. How far it
/// copied goes to `record`.
fn follow(
    mut controller: Box<dyn Controller>,
    id: &str,
    out: &mut Output,
    mut since: u64,
    record: &dyn Fn(u64, &str),
) -> Option<i32> {
    let mut answered = Instant::now();
    let mut ending = false;
    let mut recorded = Instant::now();
    loop {
        // A stopped job ends its session on the host, and follows it to its end.
        if !ending && let Some(by) = core::stopped() {
            ending = true;
            if let Err(error) = controller.stop(id) {
                let _ = writeln!(
                    out,
                    "\n✗ Stopped by {by}, but the host did not end the session {id}: {error}"
                );
                return None;
            }
        }
        let asked = controller.get(&format!("/v1/sessions/{id}/output?since={since}"));
        let output: rookery_api::Output = match asked
            .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))
        {
            Ok(output) => output,
            Err(_) if answered.elapsed() < PATIENCE => {
                std::thread::sleep(POLL * 2);
                continue;
            }
            Err(error) => {
                let _ = writeln!(
                    out,
                    "\n✗ The host stopped answering ({error}). The session {id} may still run there"
                );
                return None;
            }
        };
        answered = Instant::now();
        let data = rookery_api::unb64(&output.data_b64).unwrap_or_default();
        let _ = out.write_all(&data);
        let _ = out.flush();
        let moved = output.next_offset > since;
        since = output.next_offset;
        if moved && recorded.elapsed() > Duration::from_secs(2) {
            record(since, id);
            recorded = Instant::now();
        }
        if output.ended {
            record(since, id);
            let session = controller.get(&format!("/v1/sessions/{id}"));
            return session
                .ok()
                .and_then(|session| session["exit_code"].as_i64().map(|code| code as i32));
        }
        if !moved {
            std::thread::sleep(POLL);
        }
    }
}

impl Runner {
    /// The job a token started for a subject's action, of any kind the subject's jobs are.
    pub async fn by_token(
        &self,
        subject: Subject,
        action: &str,
        token: &str,
    ) -> Result<Option<JobInfo>, String> {
        let paths = self.paths();
        let (subject, uri) = tokio::task::spawn_blocking(move || {
            let subject = subject.by_id(&paths);
            let uri = subject_uri(&paths, &subject);
            (subject, uri)
        })
        .await
        .map_err(|e| e.to_string())?;
        for kind in kind_candidates(&subject) {
            if let Some(job) = self
                .kernel()
                .store
                .jobs()
                .by_token(kind, Some(&uri), Some(action), token)
                .await
                .map_err(|e| e.to_string())?
            {
                return Ok(Some(rookery_jobs::jobs::info(job)));
            }
        }
        Ok(None)
    }
}
