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
        let hidden_kept = match &work {
            Work::Session { .. } => hidden.clone(),
            _ => vec![],
        };
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
        runner.0.pending.lock().unwrap().insert(
            ticket,
            Pending {
                work,
                lock,
                hidden: hidden.clone(),
            },
        );
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
        } else {
            if let Some(session) = &session {
                let _ = kernel.store.jobs().set_session(job.id, session, 0).await;
            }
            // A new leader that follows the job on masks the same values.
            if !hidden_kept.is_empty()
                && let Ok(key) = kernel.store.key()
            {
                let plain = serde_json::to_vec(&hidden_kept).unwrap_or_default();
                if let Ok(sealed) = key.seal(&rookery_store::seal::row("jobs", job.id), &plain) {
                    let _ = kernel.store.jobs().set_hidden(job.id, &sealed).await;
                }
            }
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
                // What the first run masked is masked here too.
                Some((work, since)) => (
                    work,
                    None,
                    kept_hidden(&runner.kernel(), job.id).await?,
                    since,
                ),
                None => {
                    return Err(RpcError::unavailable(
                        "the server that prepared this job stopped before it ran: start the action again",
                    ));
                }
            }
        }
    };
    let stop = Arc::new(Stop::default());
    // A lock taken for the work that lapses stops it, as another may change the subject.
    if let Some(lock) = &lock {
        lock.stops(stop.clone());
    }
    // Set when this server stops leading while it follows a host's session: the session goes on
    // there, and the next leader follows it on from the offset in the log.
    let abandon = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let following = matches!(work, Work::Session { .. });
    let (lines, mut taken) = tokio::sync::mpsc::unbounded_channel::<Chunk>();
    let kernel = runner.kernel();
    let worker = {
        let (stop, abandon) = (stop.clone(), abandon.clone());
        tokio::task::spawn_blocking(move || {
            let code = core::with_stop(stop.clone(), || {
                let marks = lines.clone();
                let mut sink = Sink(lines);
                let mut out = Output::new(&mut sink, hidden);
                // How far the host's output went into the log, behind the lines it covers.
                let record = |offset: u64| {
                    let _ = marks.send(Chunk::At(offset));
                };
                let code = execute(work, &mut out, since, &record, &abandon);
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
    let mut log = Log {
        job: &job,
        pending: Vec::new(),
        held: None,
        failed: false,
    };
    let (mut asked, mut deposed) = (false, false);
    loop {
        tokio::select! {
            chunk = tokio::time::timeout(Duration::from_millis(500), taken.recv()) => match chunk {
                Ok(Some(Chunk::Data(data))) => log.take(&data).await,
                Ok(Some(Chunk::At(offset))) => log.mark(offset).await,
                Ok(None) => break,
                // A line that waits for its end is written as it is, such as a prompt.
                Err(_) => log.flush().await,
            },
            _ = job.cancelled(), if !asked => {
                asked = true;
                stop.ask("a request to cancel the job");
            }
            // The work dies with the leader: a program here is stopped, and a session on a host is
            // left to the next leader.
            _ = kernel.shutdown.cancelled(), if !deposed => {
                deposed = true;
                match following {
                    true => abandon.store(true, std::sync::atomic::Ordering::SeqCst),
                    false => {
                        stop.ask("this server, which does not lead any more");
                    }
                }
            }
        }
        // The log takes nothing more, as after a takeover: the work stops rather than run unseen.
        if log.failed && !asked {
            asked = true;
            match following {
                true => abandon.store(true, std::sync::atomic::Ordering::SeqCst),
                false => {
                    stop.ask("this server, which cannot write the job's log");
                }
            }
        }
    }
    log.flush().await;
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

/// What the work sends the log: its output, and how far a host's session went.
enum Chunk {
    Data(Vec<u8>),
    At(u64),
}

/// The job's log as it is written: a line at a time, the last complete line held back until the
/// offset that covers it comes, so the two are written in one row.
struct Log<'a> {
    job: &'a JobRun,
    pending: Vec<u8>,
    held: Option<Vec<u8>>,
    /// A write failed: the log takes no more of this run.
    failed: bool,
}

impl Log<'_> {
    async fn take(&mut self, data: &[u8]) {
        self.pending.extend_from_slice(data);
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            if let Some(before) = self.held.replace(line) {
                self.write(&before, None).await;
            }
        }
    }

    /// Everything before `offset` of the session is in: the last of it carries the offset.
    async fn mark(&mut self, offset: u64) {
        let data = json!({"session_offset": offset});
        match (self.held.take(), self.pending.is_empty()) {
            (Some(line), true) => self.write(&line, Some(data)).await,
            (held, _) => {
                if let Some(line) = held {
                    self.write(&line, None).await;
                }
                if !self.pending.is_empty() {
                    let line = std::mem::take(&mut self.pending);
                    self.write(&line, Some(data)).await;
                }
            }
        }
    }

    async fn flush(&mut self) {
        if let Some(line) = self.held.take() {
            self.write(&line, None).await;
        }
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.write(&line, None).await;
        }
    }

    async fn write(&mut self, line: &[u8], data: Option<Value>) {
        if self.failed {
            return;
        }
        let text = String::from_utf8_lossy(line);
        let text = text.trim_end_matches(['\n', '\r']);
        let level = match text.starts_with('✗') {
            true => "error",
            false => "info",
        };
        if let Err(e) = self.job.log(level, text, data).await {
            tracing::warn!(job = %self.job.id, error = %e, "writing a job's log failed: the job stops");
            self.failed = true;
        }
    }
}

/// The values the first run of a job masked, which the job keeps sealed.
async fn kept_hidden(kernel: &Kernel, id: Uuid) -> Result<Vec<String>, RpcError> {
    let Some(sealed) = kernel.store.jobs().hidden(id).await? else {
        return Ok(vec![]);
    };
    let key = kernel.store.key().map_err(|_| {
        RpcError::unavailable(
            "the job masks secrets, and this server has no secret key to read them",
        )
    })?;
    let plain = key
        .open(&rookery_store::seal::row("jobs", id), &sealed)
        .map_err(|e| RpcError::unavailable(e.to_string()))?;
    serde_json::from_slice(&plain).map_err(|e| RpcError::internal(e.to_string()))
}

/// The job's output, on its way to the log.
struct Sink(tokio::sync::mpsc::UnboundedSender<Chunk>);

impl Write for Sink {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.send(Chunk::Data(data.to_vec()));
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run the work to its end, its output to `out`: the exit code.
fn execute(
    work: Work,
    out: &mut Output,
    since: u64,
    record: &dyn Fn(u64),
    abandon: &std::sync::atomic::AtomicBool,
) -> Option<i32> {
    match work {
        Work::Function(function) => Some(if function(out) { 0 } else { 1 }),
        Work::Coded(function) => Some(function(out)),
        Work::Program(program) => core::run_program(*program, out),
        Work::Session { controller, id } => follow(controller, &id, out, since, record, abandon),
    }
}

/// How long a session's output is asked for again while there is no news,
/// and how long a host may not answer before its job gives up on it.
const POLL: Duration = Duration::from_millis(500);
const PATIENCE: Duration = Duration::from_secs(300);

/// Copy a session's output from `since` as it comes, until it has ended: its exit code. The output
/// goes on in whole lines, each batch followed by how far it reached, to `record`. A line without
/// its end waits until the session is quiet or over, so an offset never falls inside a line.
fn follow(
    mut controller: Box<dyn Controller>,
    id: &str,
    out: &mut Output,
    mut since: u64,
    record: &dyn Fn(u64),
    abandon: &std::sync::atomic::AtomicBool,
) -> Option<i32> {
    let mut answered = Instant::now();
    let mut ending = false;
    // Bytes read but not yet passed on, the start of a line, and the offset where they start.
    let mut partial: Vec<u8> = Vec::new();
    let mut quiet = 0;
    let pass = |out: &mut Output, data: &[u8], to: u64| {
        let _ = out.write_all(data);
        let _ = out.flush();
        record(to);
    };
    loop {
        if abandon.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
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
        let moved = output.next_offset > since;
        since = output.next_offset;
        partial.extend_from_slice(&data);
        quiet = if moved { 0 } else { quiet + 1 };
        if output.ended || (quiet >= 1 && !partial.is_empty()) {
            // Quiet or over: what waits goes on as it is, such as a prompt.
            let rest = std::mem::take(&mut partial);
            pass(out, &rest, since);
        } else if let Some(end) = partial.iter().rposition(|b| *b == b'\n') {
            let lines: Vec<u8> = partial.drain(..=end).collect();
            pass(out, &lines, since - partial.len() as u64);
        }
        if output.ended {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// A session that gives its output in the pieces it is given, a few bytes at a time.
    struct Pieces {
        pieces: Vec<&'static str>,
        at: usize,
        offset: u64,
    }

    impl Controller for Pieces {
        fn get(&mut self, path: &str) -> Result<Value, String> {
            if !path.contains("/output") {
                return Ok(json!({"exit_code": 0}));
            }
            let piece = self.pieces.get(self.at).copied().unwrap_or("");
            self.at += 1;
            let offset = self.offset;
            self.offset += piece.len() as u64;
            Ok(json!({
                "data_b64": rookery_api::b64(piece.as_bytes()),
                "offset": offset,
                "next_offset": self.offset,
                "next_line": 0,
                "ended": self.at > self.pieces.len(),
            }))
        }
    }

    #[test]
    fn a_session_goes_on_in_whole_lines_with_how_far_they_reach() {
        let controller = Pieces {
            pieces: vec!["one\ntw", "o\nthree\n", "a prompt: "],
            at: 0,
            offset: 0,
        };
        let marks = Mutex::new(Vec::new());
        let mut written = Vec::new();
        let code = {
            let mut out = Output::new(&mut written, vec![]);
            follow(
                Box::new(controller),
                "s1",
                &mut out,
                0,
                &|offset| marks.lock().unwrap().push(offset),
                &AtomicBool::new(false),
            )
        };
        assert_eq!(code, Some(0));
        assert_eq!(
            String::from_utf8(written).unwrap(),
            "one\ntwo\nthree\na prompt: "
        );
        // Each offset ends a line, or the prompt that waited for the session to be quiet.
        let marks = marks.into_inner().unwrap();
        assert_eq!(marks.first(), Some(&4));
        assert!(marks.contains(&14), "{marks:?}");
        assert_eq!(marks.last(), Some(&24));
    }

    #[test]
    fn a_session_is_left_to_the_next_leader() {
        let controller = Pieces {
            pieces: vec!["one\n"; 1000],
            at: 0,
            offset: 0,
        };
        let mut written = Vec::new();
        let mut out = Output::new(&mut written, vec![]);
        // Abandoned, it stops following and ends nothing on the host.
        let code = follow(
            Box::new(controller),
            "s1",
            &mut out,
            0,
            &|_| {},
            &AtomicBool::new(true),
        );
        assert_eq!(code, None);
    }
}
