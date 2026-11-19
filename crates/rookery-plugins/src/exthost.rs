//! The extension host: one Bun process for each plugin whose host half runs, started when it is first
//! needed. Each runs `host/src/main.ts` of the source, with the plugin's host half in a worker of its
//! own, and talks JSON-RPC 2.0 with the server over its standard input and output, one message a line.
//!
//! The pipes are the plugin's identity. A process speaks for its plugin alone: whatever it says of
//! another plugin, such as a `plugin` field in a message, is ignored. It starts with an empty
//! environment but for `PATH`, `HOME`, `LANG`, `LC_ALL`, `TZ`, `NODE_PATH` (the host's own packages)
//! and `ROOKERY_EXTHOST_PROTOCOL`, so the server's database address and keys are not handed to it.
//! The server and each host make themselves non-dumpable (`PR_SET_DUMPABLE`), which makes their
//! `/proc/PID/fd`, `environ` and `mem` root's, so one host cannot open another's pipes or the server's
//! memory. That is a mitigation, not a sandbox: every host runs as the server's user and reads what it
//! can read, such as the secret key file and the configuration folder. A host half is code an admin
//! installed and trusts; a sandbox of its own (a uid, landlock) is left to come.
//!
//! The server asks the process `plugins.activate`, `plugins.invoke`, `plugins.deactivate` and
//! `plugins.transform` (a migration's transform step, see [`crate::data`]), and tells it
//! `plugins.cancel` and `plugins.notify`, which carries what the kernel sent to the plugin's
//! connection, such as `events.event`. The process asks `host.hello` first, then calls the server's
//! methods, which run as the plugin on a connection of its own. It tells the server
//! `plugins.registered`, `plugins.unregistered`, `plugins.exited` and `plugins.log`.
//!
//! A host half that ends, whether its worker or its whole process, that does not stop a call past
//! its deadline, or whose process uses more memory than [`HostOptions::memory_mb`] (read from
//! `/proc/PID/status` every [`MEMORY_CHECK`]), counts against its own plugin: it is activated again with a wait that grows, up to
//! [`CRASHES`] times in [`CRASH_WINDOW`], after which the plugin is failed until an admin activates
//! it or it changes. When the server stops, each host half is deactivated, which runs its cleanups.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rookery_jobs::JobRun;
use rookery_kernel::conn::{ConnHandle, Outbound};
use rookery_kernel::{Call, Caller, Kernel};
use rookery_proto::{ErrorBody, ErrorCode, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::loaded::Loaded;
use crate::wire::PluginState;

/// How a plugin's host half that keeps ending is given up on.
pub const CRASHES: usize = 5;
pub const CRASH_WINDOW: Duration = Duration::from_secs(60);
/// How long a new process may take to say hello, and a host half to activate.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(30);
const DEACTIVATE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a handler has to stop once its call's deadline passed, before its host half is ended.
pub const CANCEL_GRACE: Duration = Duration::from_secs(3);
/// How long one transform step of a migration may run.
pub const TRANSFORM_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// The version of this protocol.
pub const PROTOCOL: &str = "1";
/// How often a host's memory is read.
pub const MEMORY_CHECK: Duration = Duration::from_secs(2);
/// What of the server's environment a host half gets.
const MAX_CALLS: usize = 64;
const MAX_STREAMS: usize = 32;

const KEPT_ENV: &[&str] = &["PATH", "HOME", "LANG", "LC_ALL", "TZ"];

/// How the server starts the extension host.
#[derive(Debug, Clone)]
pub struct HostOptions {
    /// The Bun program.
    pub bun: PathBuf,
    /// `host/src/main.ts`. Without it no host half runs, and a plugin with one fails to activate.
    pub entry: Option<PathBuf>,
    /// The most memory a plugin's host process may hold (its resident set), in MiB. One that holds
    /// more is ended, as a crash.
    pub memory_mb: u32,
}

impl Default for HostOptions {
    fn default() -> Self {
        HostOptions {
            bun: PathBuf::from("bun"),
            entry: None,
            memory_mb: 512,
        }
    }
}

/// Something a host half registered: a method of `rookery.rpc.handle`, a command and the like.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Registration {
    pub kind: String,
    pub key: String,
    /// Who may call it: `own`, anyone signed in, or `admin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub methods: BTreeMap<String, ServiceMethod>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceMethod {
    #[serde(default)]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

#[derive(Clone)]
pub(crate) enum Invocation {
    Stream {
        items: mpsc::Sender<Value>,
        cancelled: CancellationToken,
    },
    Job(JobRun),
}

struct InvocationGuard {
    link: Link,
    id: String,
}

impl Drop for InvocationGuard {
    fn drop(&mut self) {
        if let Some(Invocation::Stream { cancelled, .. }) =
            self.link.0.invocations.lock().unwrap().remove(&self.id)
        {
            cancelled.cancel();
        }
    }
}

#[derive(Default)]
struct Slot {
    state: Option<PluginState>,
    error: Option<ErrorBody>,
    registrations: Vec<Registration>,
    /// The process it is active in.
    generation: u64,
    /// It is to run: it is activated again when it ends.
    wanted: bool,
    crashes: Vec<Instant>,
}

/// A plugin's running process: its link, and the plugin's connection, which lives as long.
struct Proc {
    link: Link,
    conn: ConnHandle,
}

struct Capacity {
    incoming: Arc<Semaphore>,
    calls: Arc<Semaphore>,
    streams: Arc<Semaphore>,
}

impl Default for Capacity {
    fn default() -> Self {
        Self {
            incoming: Arc::new(Semaphore::new(MAX_CALLS)),
            calls: Arc::new(Semaphore::new(MAX_CALLS)),
            streams: Arc::new(Semaphore::new(MAX_STREAMS)),
        }
    }
}

fn reserve(semaphore: &Arc<Semaphore>) -> Result<OwnedSemaphorePermit, RpcError> {
    semaphore
        .clone()
        .try_acquire_owned()
        .map_err(|_| RpcError::unavailable("the plugin has reached its call or stream limit"))
}

pub(crate) struct InvocationPermits {
    _call: OwnedSemaphorePermit,
    pub stream: Option<Arc<OwnedSemaphorePermit>>,
    pub caller_stream: Option<Arc<OwnedSemaphorePermit>>,
}

pub(crate) struct Target {
    pub link: Link,
    plugin: String,
    pub registration: Registration,
}

pub(crate) struct ExtHost {
    options: HostOptions,
    generation: AtomicU64,
    slots: Mutex<HashMap<String, Slot>>,
    procs: Mutex<HashMap<String, Proc>>,
    capacity: Mutex<HashMap<String, Arc<Capacity>>>,
    gates: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// The server is stopping: no host half starts, so none is left running past the snapshot that
    /// stops them, to race the store closing.
    stopping: AtomicBool,
}

impl ExtHost {
    pub fn new(options: HostOptions) -> ExtHost {
        ExtHost {
            options,
            generation: AtomicU64::new(0),
            slots: Mutex::default(),
            procs: Mutex::default(),
            capacity: Mutex::default(),
            gates: Mutex::default(),
            stopping: AtomicBool::new(false),
        }
    }

    /// Refuses every start of a host half from now on, as the server stops.
    pub fn stopping(&self) {
        let _procs = self.procs.lock().unwrap();
        self.stopping.store(true, Ordering::SeqCst);
    }

    fn capacity(&self, plugin: &str) -> Arc<Capacity> {
        self.capacity
            .lock()
            .unwrap()
            .entry(plugin.to_owned())
            .or_default()
            .clone()
    }

    pub(crate) fn reserve_invocation(
        &self,
        plugin: &str,
        stream: bool,
    ) -> Result<InvocationPermits, RpcError> {
        let capacity = self.capacity(plugin);
        Ok(InvocationPermits {
            _call: reserve(&capacity.calls)?,
            stream: stream
                .then(|| reserve(&capacity.streams))
                .transpose()?
                .map(Arc::new),
            caller_stream: None,
        })
    }

    fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// The state of a plugin's host half, and why it failed; `None` while it never ran.
    pub fn state(&self, plugin: &str) -> (Option<PluginState>, Option<ErrorBody>) {
        let slots = self.slots.lock().unwrap();
        match slots.get(plugin) {
            Some(slot) => (slot.state, slot.error.clone()),
            None => (None, None),
        }
    }

    /// The link of a plugin's running process.
    pub fn running(&self, plugin: &str) -> Option<Link> {
        let procs = self.procs.lock().unwrap();
        procs.get(plugin).map(|p| p.link.clone())
    }

    /// Serializes starting, activating, deactivating and transforming one plugin.
    pub fn gate(&self, plugin: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.gates
            .lock()
            .unwrap()
            .entry(plugin.to_owned())
            .or_default()
            .clone()
    }

    /// The link of a plugin whose host half is active in its running process.
    pub fn active_link(&self, plugin: &str) -> Option<Link> {
        let procs = self.procs.lock().unwrap();
        let link = &procs.get(plugin)?.link;
        let slots = self.slots.lock().unwrap();
        let slot = slots.get(plugin)?;
        (link.is_accepting()
            && slot.state == Some(PluginState::Active)
            && slot.generation == link.generation())
        .then(|| link.clone())
    }

    pub(crate) fn target(
        &self,
        plugin: &str,
        link: Link,
        kind: &str,
        key: &str,
    ) -> Result<Target, RpcError> {
        let slots = self.slots.lock().unwrap();
        let slot = slots
            .get(plugin)
            .filter(|slot| {
                slot.state == Some(PluginState::Active) && slot.generation == link.generation()
            })
            .filter(|_| link.is_accepting())
            .ok_or_else(|| RpcError::unavailable("the host half changed"))?;
        let registration = slot
            .registrations
            .iter()
            .find(|r| r.kind == kind && r.key == key)
            .ok_or_else(|| RpcError::not_found(format!("{plugin} has no {kind} {key}")))?
            .clone();
        Ok(Target {
            link,
            plugin: plugin.to_owned(),
            registration,
        })
    }

    fn plugin_conn(&self, plugin: &str, generation: u64) -> Option<ConnHandle> {
        let procs = self.procs.lock().unwrap();
        procs
            .get(plugin)
            .filter(|p| p.link.generation() == generation)
            .map(|p| p.conn.clone())
    }

    fn set(&self, plugin: &str, change: impl FnOnce(&mut Slot)) {
        let mut slots = self.slots.lock().unwrap();
        change(slots.entry(plugin.to_owned()).or_default());
    }

    /// The plugin's running process, started when there is none. The caller holds its gate.
    async fn start(
        &self,
        kernel: &Kernel,
        plugins: &Arc<crate::Plugins>,
        plugin: &str,
    ) -> Result<Link, RpcError> {
        if self.is_stopping() {
            return Err(RpcError::unavailable(
                "the server is stopping, so no host half starts",
            ));
        }
        if let Some(link) = self.running(plugin) {
            if !link.is_stopping() {
                return Ok(link);
            }
            link.reaped().await;
        }
        let (link, said_hello) = {
            // Shutdown takes this same lock before its snapshot.
            let mut procs = self.procs.lock().unwrap();
            if self.is_stopping() {
                return Err(RpcError::unavailable(
                    "the server is stopping, so no host half starts",
                ));
            }
            let Some(entry) = &self.options.entry else {
                return Err(RpcError::unavailable(
                    "no extension host is set up on this server (ROOKERY_EXTHOST), so no host half can run",
                ));
            };
            let child = spawn(&self.options, entry).map_err(|e| {
                RpcError::unavailable(format!(
                    "the extension host of {plugin} could not start with {}: {e}",
                    self.options.bun.display()
                ))
            })?;
            let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
            let (out, rx) = mpsc::unbounded_channel::<String>();
            let link = Link(Arc::new(LinkInner {
                generation,
                pid: child.id(),
                out,
                pending: Mutex::default(),
                invocations: Mutex::default(),
                streams: Mutex::default(),
                next: AtomicI64::new(1),
                stop: CancellationToken::new(),
                draining: AtomicBool::new(false),
                reaped: CancellationToken::new(),
                crash: Mutex::default(),
                plugin: OnceLock::new(),
                capacity: self.capacity(plugin),
                credentials: Mutex::default(),
            }));
            let (conn, notices) = ConnHandle::detached(kernel, Caller::Plugin(plugin.to_owned()));
            tokio::spawn(forward(notices, link.clone(), plugin.to_owned()));
            procs.insert(
                plugin.to_owned(),
                Proc {
                    link: link.clone(),
                    conn,
                },
            );
            let (ready, said_hello) = oneshot::channel();
            tokio::spawn(run(
                kernel.clone(),
                plugins.clone(),
                plugin.to_owned(),
                child,
                link.clone(),
                rx,
                ready,
            ));
            (link, said_hello)
        };
        match tokio::time::timeout(CONNECT_TIMEOUT, said_hello).await {
            Ok(Ok(())) => Ok(link),
            _ => {
                link.stop();
                link.reaped().await;
                Err(RpcError::unavailable(format!(
                    "the extension host of {plugin} did not start"
                )))
            }
        }
    }

    /// Lets processes clean up, then kills and reaps any remaining processes before returning.
    pub async fn stop_all(&self, kernel: &Kernel, limit: Duration) {
        self.stopping();
        let procs: Vec<_> = self
            .procs
            .lock()
            .unwrap()
            .iter()
            .map(|(plugin, proc)| (plugin.clone(), proc.link.clone()))
            .collect();
        let all = futures_util::future::join_all(
            procs
                .iter()
                .map(|(plugin, _)| self.deactivate(kernel, plugin)),
        );
        if tokio::time::timeout(limit, all).await.is_err() {
            tracing::warn!("host halves did not finish their cleanups in time");
        }
        for (_, link) in &procs {
            link.stop();
        }
        futures_util::future::join_all(procs.iter().map(|(_, link)| link.reaped())).await;
    }

    /// Runs a transform step of a plugin's migration in its process, in a worker of its own. Its calls
    /// of the server go as the plugin, whether or not its host half runs.
    pub async fn transform(
        &self,
        kernel: &Kernel,
        plugins: Arc<crate::Plugins>,
        plugin: &str,
        params: Value,
        limit: Duration,
    ) -> Result<(), RpcError> {
        let gate = self.gate(plugin);
        let _one = gate.lock().await;
        let link = self.start(kernel, &plugins, plugin).await?;
        let done = tokio::time::timeout(limit, link.request("plugins.transform", params)).await;
        let idle = self.active_link(plugin).is_none();
        match done {
            Ok(done) => {
                if idle {
                    link.stop();
                    link.reaped().await;
                }
                done.map(|_| ())
            }
            Err(_) => {
                // Its worker is still running: the process goes, and with it the step.
                link.stop();
                link.reaped().await;
                Err(RpcError::timeout(format!(
                    "the transform step of {plugin} did not end in {} seconds",
                    limit.as_secs()
                )))
            }
        }
    }

    /// Starts a plugin's host half in its process, starting the process when it does not run.
    pub async fn activate(
        &self,
        kernel: &Kernel,
        plugins: Arc<crate::Plugins>,
        loaded: Arc<Loaded>,
    ) -> Result<Link, RpcError> {
        let plugin = &loaded.id;
        let gate = self.gate(plugin);
        let _one = gate.lock().await;
        if self.is_stopping() {
            return Err(RpcError::unavailable(
                "the server is stopping, so no host half starts",
            ));
        }
        plugins.with_current(&loaded, || Ok(()))?;
        if let Some(link) = self.active_link(plugin) {
            link.check_plugin(&loaded)?;
            return Ok(link);
        }
        self.set(plugin, |slot| {
            slot.state = Some(PluginState::Activating);
            slot.error = None;
            slot.wanted = true;
        });
        let activated = async {
            let link = self.start(kernel, &plugins, plugin).await?;
            let params = json!({"plugin": plugin, "dir": loaded.dir, "manifest": loaded.manifest});
            match link
                .call::<Activated>("plugins.activate", params, ACTIVATE_TIMEOUT)
                .await
                .and_then(|activated| {
                    for registration in &activated.registrations {
                        check_registration(&loaded, registration)?;
                    }
                    Ok(activated)
                }) {
                Ok(activated) => Ok((link, activated.registrations)),
                Err(e) => {
                    link.stop();
                    link.reaped().await;
                    Err(e)
                }
            }
        }
        .await;
        match activated {
            Ok((link, registrations)) => {
                let activate = plugins.with_current(&loaded, || {
                    let procs = self.procs.lock().unwrap();
                    if link.is_stopping() || !procs.contains_key(plugin) {
                        return Err(RpcError::unavailable(
                            "the host half ended during activation",
                        ));
                    }
                    link.0.plugin.set(loaded.clone()).ok();
                    self.set(plugin, |slot| {
                        slot.state = Some(PluginState::Active);
                        slot.generation = link.generation();
                        slot.registrations = registrations;
                    });
                    Ok(())
                });
                if let Err(error) = activate {
                    link.stop();
                    link.reaped().await;
                    return Err(error);
                }
                tracing::info!(plugin, pid = ?link.pid(), "a plugin's host half is active");
                Ok(link)
            }
            Err(e) => {
                tracing::warn!(plugin, error = %e, "a plugin's host half did not activate");
                self.set(plugin, |slot| {
                    slot.state = Some(PluginState::Failed);
                    slot.error = Some(e.body());
                    slot.wanted = false;
                });
                Err(e)
            }
        }
    }

    /// Stops a plugin's host half and its process, after its cleanups. It is not started again.
    pub async fn deactivate(&self, _kernel: &Kernel, plugin: &str) {
        let gate = self.gate(plugin);
        let _one = gate.lock().await;
        self.slots.lock().unwrap().remove(plugin);
        let Some(link) = self.running(plugin) else {
            return;
        };
        link.0.draining.store(true, Ordering::Release);
        let _ = link
            .call::<Value>(
                "plugins.deactivate",
                json!({"plugin": plugin}),
                DEACTIVATE_TIMEOUT,
            )
            .await;
        link.stop();
        link.reaped().await;
    }

    /// Checks the registration again and sends the request on its original pipe before returning.
    pub(crate) fn begin(
        &self,
        call: &Call,
        target: Target,
        params: Value,
        invocation: Option<Invocation>,
        permits: InvocationPermits,
    ) -> Result<InvocationResult, RpcError> {
        let Target {
            link,
            plugin,
            registration,
        } = target;
        let kind = &registration.kind;
        let key = &registration.key;
        let procs = self.procs.lock().unwrap();
        let slots = self.slots.lock().unwrap();
        let current = procs
            .get(&plugin)
            .is_some_and(|proc| proc.link.generation() == link.generation())
            && slots.get(&plugin).is_some_and(|slot| {
                slot.generation == link.generation()
                    && slot.state == Some(PluginState::Active)
                    && slot.registrations.contains(&registration)
            });
        if !current || !link.is_accepting() || self.is_stopping() {
            return Err(RpcError::unavailable(
                "the host half changed during the call",
            ));
        }
        match registration.level.as_deref() {
            _ if kind != "rpc" => {}
            // A person signed in, or the server's own machine; not another plugin.
            Some("own") if !matches!(call.caller, Caller::Plugin(_)) => {}
            Some("own") => {
                return Err(RpcError::forbidden(format!(
                    "{kind} {key} of {plugin} is for people, and the caller is a plugin"
                )));
            }
            None => {
                return Err(RpcError::forbidden(format!(
                    "{kind} {key} of {plugin} says no level, so nobody may call it"
                )));
            }
            Some("admin") => call.require_admin().map_err(|_| {
                RpcError::forbidden(format!("{kind} {key} of {plugin} is for admins"))
            })?,
            Some(other) => {
                return Err(RpcError::forbidden(format!(
                    "{kind} {key} of {plugin} has the level {other:?}, which Rookery does not know"
                )));
            }
        }
        let call_id = uuid::Uuid::now_v7().to_string();
        let left = call.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(RpcError::timeout("the call's deadline passed"));
        }
        let deadline = chrono::Utc::now().timestamp_millis()
            + i64::try_from(left.as_millis()).unwrap_or(i64::MAX / 2);
        let ctx = json!({
            "callId": call_id,
            "user": call.user(),
            "principal": call.principal(),
            "deadline": deadline,
        });
        let stream = matches!(invocation, Some(Invocation::Stream { .. }));
        let invoke = json!({"plugin": plugin, "kind": kind, "key": key, "params": params, "ctx": ctx, "stream": stream});
        let job = match &invocation {
            Some(Invocation::Job(run)) => Some(run.clone()),
            _ => None,
        };
        let guard = invocation.map(|invocation| {
            link.0
                .invocations
                .lock()
                .unwrap()
                .insert(call_id.clone(), invocation);
            InvocationGuard {
                link: link.clone(),
                id: call_id.clone(),
            }
        });
        let request = link.request("plugins.invoke", invoke);
        let deadline = call.deadline;
        let (mut tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let _permits = permits;
            let mut request = Box::pin(request);
            let cancelled = async {
                match &job {
                    Some(job) => job.cancelled().await,
                    None => std::future::pending().await,
                }
            };
            let reason = tokio::select! {
                biased;
                done = &mut request => {
                    drop(guard);
                    if job.is_some() && !link.is_accepting() {
                        link.reaped().await;
                    }
                    let _ = tx.send(done);
                    return;
                }
                _ = tokio::time::sleep_until(deadline.into()) => "timeout",
                _ = cancelled => "cancelled",
                _ = tx.closed() => "caller-closed",
            };
            link.notify(
                "plugins.cancel",
                json!({"plugin": plugin, "callId": call_id, "reason": reason}),
            );
            // Releasing the stream sender also wakes a write waiting for reader credit.
            drop(guard);
            let error = if reason == "timeout" {
                RpcError::timeout(format!(
                    "the handler of {plugin} did not answer before the call's deadline"
                ))
            } else {
                RpcError::unavailable(format!("the call to {plugin} was cancelled"))
            };
            // Only the job queue needs to wait for teardown. It holds and renews the claim until then.
            let finish_job = if job.is_some() {
                Some(tx)
            } else {
                let _ = tx.send(Err(error.clone()));
                None
            };
            if tokio::time::timeout(CANCEL_GRACE, request).await.is_err() {
                link.crash(
                    RpcError::timeout(format!(
                        "{plugin} did not stop a call {CANCEL_GRACE:?} past its deadline"
                    ))
                    .body(),
                );
            }
            if !link.is_accepting() {
                link.reaped().await;
            }
            if let Some(tx) = finish_job {
                let _ = tx.send(Err(error));
            }
        });
        Ok(InvocationResult(rx))
    }

    /// Counts a crash of a plugin's host half. Says how long to wait before it is activated again, or
    /// `None` when it ended too often or is not wanted.
    fn exited(&self, plugin: &str, generation: u64, error: Option<ErrorBody>) -> Option<Duration> {
        let now = Instant::now();
        let mut slots = self.slots.lock().unwrap();
        let slot = slots.get_mut(plugin)?;
        slot.crashes
            .retain(|at| now.duration_since(*at) < CRASH_WINDOW);
        slot.crashes.push(now);
        slot.generation = generation;
        slot.registrations.clear();
        let crashes = slot.crashes.len();
        if slot.wanted && crashes < CRASHES {
            slot.state = Some(PluginState::Inactive);
            slot.error = error;
            Some(Duration::from_millis(250 * 2u64.pow(crashes as u32 - 1)))
        } else {
            let said = error.map(|e| e.message).unwrap_or_default();
            slot.state = Some(PluginState::Failed);
            slot.wanted = false;
            slot.error = Some(ErrorBody {
                code: ErrorCode::Unavailable,
                message: format!(
                    "the host half of {plugin} ended {crashes} times in {} seconds, and is not started again until an admin activates it: {said}",
                    CRASH_WINDOW.as_secs()
                ),
                details: None,
            });
            None
        }
    }

    fn registered(&self, plugin: &str, registration: Registration) {
        self.set(plugin, |slot| {
            slot.registrations
                .retain(|r| !(r.kind == registration.kind && r.key == registration.key));
            slot.registrations.push(registration);
        });
    }

    fn unregistered(&self, plugin: &str, kind: &str, key: &str) {
        self.set(plugin, |slot| {
            slot.registrations
                .retain(|r| !(r.kind == kind && r.key == key));
        });
    }
}

/// Removes a reaped generation before releasing its invocations or allowing a new process to start.
fn reaped(
    kernel: &Kernel,
    plugins: &Arc<crate::Plugins>,
    plugin: &str,
    link: &Link,
    error: Option<ErrorBody>,
) {
    let host = &plugins.host;
    let generation = link.generation();
    let (proc, again) = {
        let mut procs = host.procs.lock().unwrap();
        let proc = procs
            .remove(plugin)
            .expect("the process stays registered until it is reaped");
        let again = if let Some(error) =
            error.filter(|_| !host.is_stopping() && !kernel.shutdown.is_cancelled())
        {
            tracing::warn!(plugin, message = %error.message, "a plugin's host half ended");
            host.exited(plugin, generation, Some(error))
        } else {
            if let Some(slot) = host.slots.lock().unwrap().get_mut(plugin)
                && slot.generation == generation
            {
                slot.state = Some(PluginState::Inactive);
                slot.registrations.clear();
            }
            None
        };
        (proc, again)
    };
    proc.conn.close_detached(kernel);
    link.fail_all();
    link.0.reaped.cancel();
    plugins.changed(kernel, &[plugin.to_owned()]);
    if let Some(wait) = again {
        let (kernel, plugins, plugin) = (kernel.clone(), plugins.clone(), plugin.to_owned());
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            if host_restart_pending(&plugins.host, &plugin, generation) {
                let _ = plugins.activate_as(&kernel, &plugin, false).await;
            }
        });
    }
}

fn host_restart_pending(host: &ExtHost, plugin: &str, generation: u64) -> bool {
    host.slots.lock().unwrap().get(plugin).is_some_and(|slot| {
        slot.generation == generation && slot.wanted && slot.state == Some(PluginState::Inactive)
    })
}

pub(crate) struct InvocationResult(oneshot::Receiver<Result<Value, RpcError>>);

impl InvocationResult {
    pub(crate) async fn result(&mut self) -> Result<Value, RpcError> {
        (&mut self.0)
            .await
            .unwrap_or_else(|_| Err(RpcError::unavailable("the call went away")))
    }
}

struct HostStream {
    source: tokio::sync::Mutex<crate::services::ServiceStream>,
    closed: CancellationToken,
    _capacity: Arc<OwnedSemaphorePermit>,
}

#[derive(Deserialize)]
struct Activated {
    #[serde(default)]
    registrations: Vec<Registration>,
}

type Pending = Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>;

/// The server's side of the pipes of one plugin's process.
#[derive(Clone)]
pub struct Link(Arc<LinkInner>);

struct LinkInner {
    generation: u64,
    pid: Option<u32>,
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
    invocations: Mutex<HashMap<String, Invocation>>,
    streams: Mutex<HashMap<String, Arc<HostStream>>>,
    next: AtomicI64,
    stop: CancellationToken,
    draining: AtomicBool,
    reaped: CancellationToken,
    crash: Mutex<Option<ErrorBody>>,
    plugin: OnceLock<Arc<Loaded>>,
    capacity: Arc<Capacity>,
    credentials: Mutex<Vec<String>>,
}

impl Link {
    pub fn generation(&self) -> u64 {
        self.0.generation
    }

    pub fn pid(&self) -> Option<u32> {
        self.0.pid
    }

    fn send(&self, message: Value) -> bool {
        self.0.out.send(message.to_string()).is_ok()
    }

    fn stop(&self) {
        self.0.stop.cancel();
    }

    fn crash(&self, error: ErrorBody) {
        self.0.crash.lock().unwrap().get_or_insert(error);
        self.stop();
    }

    async fn reaped(&self) {
        self.0.reaped.cancelled().await;
    }

    fn is_stopping(&self) -> bool {
        self.0.stop.is_cancelled()
    }

    fn is_accepting(&self) -> bool {
        !self.is_stopping() && !self.0.draining.load(Ordering::Acquire)
    }

    pub(crate) fn check_plugin(&self, expected: &Arc<Loaded>) -> Result<(), RpcError> {
        if self
            .0
            .plugin
            .get()
            .is_some_and(|loaded| Arc::ptr_eq(loaded, expected))
        {
            Ok(())
        } else {
            Err(RpcError::unavailable("the plugin changed during the call"))
        }
    }

    /// A request. It answers `UNAVAILABLE` when the process goes away first, and has no time limit of
    /// its own.
    pub fn request(
        &self,
        method: &str,
        params: Value,
    ) -> impl Future<Output = Result<Value, RpcError>> + Send + 'static {
        let (tx, rx) = oneshot::channel();
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        let sent = {
            let mut pending = self.0.pending.lock().unwrap();
            if self.is_stopping() {
                false
            } else {
                pending.insert(id, tx);
                let sent = self
                    .send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
                if !sent {
                    pending.remove(&id);
                }
                sent
            }
        };
        async move {
            if !sent {
                return Err(RpcError::unavailable("the extension host is not running"));
            }
            rx.await
                .unwrap_or_else(|_| Err(RpcError::unavailable("the extension host went away")))
        }
    }

    pub async fn call<R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
        limit: Duration,
    ) -> Result<R, RpcError> {
        let value = tokio::time::timeout(limit, self.request(method, params))
            .await
            .map_err(|_| {
                RpcError::timeout(format!(
                    "the extension host did not answer {method} in {} seconds",
                    limit.as_secs()
                ))
            })??;
        serde_json::from_value(value).map_err(|e| RpcError::internal(format!("{method}: {e}")))
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn respond(&self, id: Value, result: Result<Value, RpcError>) {
        let message = match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        };
        self.send(message);
    }

    fn resolve(&self, message: &Value) {
        let Some(id) = message.get("id").and_then(Value::as_i64) else {
            return;
        };
        let Some(tx) = self.0.pending.lock().unwrap().remove(&id) else {
            return;
        };
        let result = match message.get("error") {
            Some(error) => {
                let mut error = serde_json::from_value::<RpcError>(error.clone())
                    .unwrap_or_else(|_| RpcError::internal("the plugin returned an invalid error"));
                error.message = self.redact(&error.message);
                if let Some(details) = error.data.as_mut().and_then(|data| data.details.as_mut()) {
                    self.redact_details(details);
                }
                Err(error)
            }
            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = tx.send(result);
    }

    fn remember_credential(&self, result: &Result<Value, RpcError>) {
        if let Some(credential) = result
            .as_ref()
            .ok()
            .and_then(|result| result.get("credential"))
            .and_then(Value::as_str)
            .filter(|credential| !credential.is_empty())
        {
            let mut credentials = self.0.credentials.lock().unwrap();
            if !credentials.iter().any(|known| known == credential) {
                let quoted = serde_json::to_string(credential).expect("a string serializes");
                let escaped = &quoted[1..quoted.len() - 1];
                if escaped != credential {
                    credentials.push(escaped.to_owned());
                }
                credentials.push(credential.to_owned());
                // Log formatters can emit escaped credentials, and one value can contain another.
                credentials.sort_by_key(|value| std::cmp::Reverse(value.len()));
            }
        }
    }

    fn redact(&self, text: &str) -> String {
        let mut sanitized = text.to_owned();
        for credential in self.0.credentials.lock().unwrap().iter() {
            if sanitized.contains(credential) {
                sanitized = sanitized.replace(credential, "[redacted]");
            }
        }
        sanitized
    }

    fn redact_details(&self, details: &mut Value) {
        match details {
            Value::String(text) => *text = self.redact(text),
            Value::Array(values) => values.iter_mut().for_each(|item| self.redact_details(item)),
            Value::Object(values) => {
                for (key, mut item) in std::mem::take(values) {
                    self.redact_details(&mut item);
                    values.insert(self.redact(&key), item);
                }
            }
            _ => {}
        }
    }

    fn close_stream(&self, id: &str) {
        if let Some(stream) = self.0.streams.lock().unwrap().remove(id) {
            stream.closed.cancel();
        }
    }

    fn fail_all(&self) {
        for (_, stream) in self.0.streams.lock().unwrap().drain() {
            stream.closed.cancel();
        }
        let pending: Vec<_> = self.0.pending.lock().unwrap().drain().collect();
        for (_, tx) in pending {
            let _ = tx.send(Err(RpcError::unavailable("the extension host went away")));
        }
    }
}

/// What the kernel sends a plugin's connection goes to its process.
async fn forward(mut rx: mpsc::Receiver<Outbound>, link: Link, plugin: String) {
    while let Some(out) = rx.recv().await {
        if link.is_stopping() {
            break;
        }
        let Outbound::Text(text) = out else {
            continue;
        };
        let Ok(message) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            link.notify(
                "plugins.notify",
                json!({"plugin": plugin, "method": method, "params": params}),
            );
        }
    }
}

/// Makes the server non-dumpable, once: its `/proc/PID/fd`, `environ` and `mem` become root's, out of
/// reach of the hosts, which run as the same user.
fn undumpable() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        #[cfg(target_os = "linux")]
        // SAFETY: prctl with PR_SET_DUMPABLE changes only a flag of this process.
        if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
            tracing::warn!(
                error = %std::io::Error::last_os_error(),
                "the server could not make itself non-dumpable; hosts may read its memory"
            );
        }
    });
}

/// The resident memory of a process, in MiB, from `/proc/PID/status`.
fn resident_mb(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

fn spawn(options: &HostOptions, entry: &Path) -> std::io::Result<Child> {
    undumpable();
    // A plugin's folder has no node_modules of its own to find @rookery/sdk in: it gets the host
    // package's workspace's (host/src/main.ts → node_modules beside host/), which its worker inherits.
    let node_path = std::path::absolute(entry)
        .ok()
        .and_then(|e| e.ancestors().nth(3).map(|root| root.join("node_modules")))
        .unwrap_or_default();
    let mut cmd = Command::new(&options.bun);
    cmd.arg(entry).env_clear();
    for name in KEPT_ENV {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    cmd.env("NODE_PATH", node_path)
        .env("ROOKERY_EXTHOST_PROTOCOL", PROTOCOL)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(target_os = "linux")]
    // SAFETY: prctl is async-signal-safe; it asks the kernel to end the child when the server ends.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    cmd.spawn()
}

/// One process's life: its messages until it ends, is stopped or the server stops.
async fn run(
    kernel: Kernel,
    plugins: Arc<crate::Plugins>,
    plugin: String,
    mut child: Child,
    link: Link,
    mut rx: mpsc::UnboundedReceiver<String>,
    ready: oneshot::Sender<()>,
) {
    let host = &plugins.host;
    let generation = link.generation();
    tracing::info!(plugin, pid = ?child.id(), generation, "a plugin's extension host started");
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let writer = tokio::spawn(async move {
        while let Some(mut line) = rx.recv().await {
            line.push('\n');
            if stdin.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let stderr = child.stderr.take().map(|err| {
        let plugin = plugin.clone();
        let link = link.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = link.redact(&line);
                tracing::info!(target: "rookery::exthost", plugin, "{line}");
            }
        })
    });
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut lines = BufReader::new(stdout).lines();
    let mut ready = Some(ready);
    let stop = link.0.stop.clone();
    let mut leaving = false;
    let mut memory = tokio::time::interval(MEMORY_CHECK);
    let limit = u64::from(host.options.memory_mb);
    let pid = child.id();
    let mut over: Option<ErrorBody> = None;
    loop {
        let line = tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            _ = memory.tick() => {
                if let Some(used) = pid.and_then(resident_mb).filter(|used| *used > limit) {
                    over = Some(ErrorBody {
                        code: ErrorCode::Unavailable,
                        message: format!(
                            "the extension host of {plugin} held {used} MiB, more than its {limit} MiB"
                        ),
                        details: None,
                    });
                    break;
                }
                continue;
            }
            _ = kernel.shutdown.cancelled(), if !leaving => {
                link.0.draining.store(true, Ordering::Release);
                // A host half's cleanups run before the server goes; this loop reads their answer.
                leaving = true;
                let (link, plugin) = (link.clone(), plugin.clone());
                tokio::spawn(async move {
                    let _ = link
                        .call::<Value>("plugins.deactivate", json!({"plugin": plugin}), DEACTIVATE_TIMEOUT)
                        .await;
                    link.stop();
                });
                continue;
            }
            line = lines.next_line() => line,
        };
        let Ok(Some(line)) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            tracing::warn!(target: "rookery::exthost", plugin, "invalid extension host protocol message");
            continue;
        };
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").filter(|id| !id.is_null()).cloned();
        match (method, id) {
            (Some("host.hello"), Some(id)) => {
                link.respond(id, Ok(json!({"protocol": PROTOCOL})));
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
            }
            (Some(method), Some(id)) => {
                // The pipe says whose call this is, whatever the message says.
                let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
                let Some(conn) = host.plugin_conn(&plugin, generation) else {
                    link.respond(id, Err(RpcError::unavailable("this host half was stopped")));
                    continue;
                };
                let call = Call::new(kernel.clone(), Caller::Plugin(plugin.clone()), Some(conn));
                let permit = match reserve(&link.0.capacity.incoming) {
                    Ok(permit) => permit,
                    Err(error) => {
                        link.respond(id, Err(error));
                        continue;
                    }
                };
                let link = link.clone();
                let plugins = plugins.clone();
                let method = method.to_owned();
                tokio::spawn(async move {
                    let _permit = permit;
                    let done = host_request(&call, &plugins, &link, &method, params).await;
                    if method == "connections.resolve" {
                        link.remember_credential(&done);
                    }
                    link.respond(id, done);
                });
            }
            (Some(method), None) => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                notified(&plugins, &plugin, &link, method, params);
            }
            (None, Some(_)) if !link.is_stopping() => link.resolve(&message),
            (None, _) => {}
        }
    }
    let requested = link.is_stopping();
    link.stop();
    writer.abort();
    let _ = child.start_kill();
    let status = child.wait().await;
    if let Some(stderr) = stderr {
        stderr.abort();
    }
    tracing::info!(
        plugin,
        ?status,
        generation,
        "a plugin's extension host ended"
    );
    let error = link.0.crash.lock().unwrap().take().or(over).or_else(|| {
        (!requested).then(|| ErrorBody {
            code: ErrorCode::Unavailable,
            message: format!("the extension host of {plugin} ended ({status:?})"),
            details: None,
        })
    });
    reaped(&kernel, &plugins, &plugin, &link, error);
}

/// A notification of a plugin's process, about that plugin alone.
fn notified(plugins: &crate::Plugins, plugin: &str, link: &Link, method: &str, params: Value) {
    let host = &plugins.host;
    if link.is_stopping() || host.plugin_conn(plugin, link.generation()).is_none() {
        return;
    }
    match method {
        "plugins.exited" => {
            let error = params.get("error").cloned().and_then(|mut error| {
                link.redact_details(&mut error);
                serde_json::from_value(error).ok()
            });
            link.crash(
                error.unwrap_or_else(|| RpcError::unavailable("the plugin worker ended").body()),
            );
        }
        "plugins.registered" => {
            if let Some(registration) = params
                .get("registration")
                .and_then(|r| serde_json::from_value(r.clone()).ok())
            {
                let Some(loaded) = link.0.plugin.get() else {
                    return;
                };
                match check_registration(loaded, &registration) {
                    Ok(()) => host.registered(plugin, registration),
                    Err(e) => {
                        tracing::warn!(plugin, error = %link.redact(&e.message), "ignoring a plugin registration")
                    }
                }
            }
        }
        "plugins.unregistered" => {
            let text = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or("");
            host.unregistered(plugin, text("kind"), text("key"));
        }
        "plugins.log" => {
            let message = link.redact(params.get("message").and_then(Value::as_str).unwrap_or(""));
            match params.get("level").and_then(Value::as_str) {
                Some("error") => tracing::error!(target: "rookery::plugin", plugin, "{message}"),
                Some("warn") => tracing::warn!(target: "rookery::plugin", plugin, "{message}"),
                Some("debug") => tracing::debug!(target: "rookery::plugin", plugin, "{message}"),
                _ => tracing::info!(target: "rookery::plugin", plugin, "{message}"),
            }
        }
        _ => {}
    }
}

/// Check both activation and later registrations against the manifest of the process on the pipe.
fn check_registration(loaded: &Loaded, registration: &Registration) -> Result<(), RpcError> {
    let plugin = &loaded.id;
    let key = &registration.key;
    let declared = match registration.kind.as_str() {
        "service" => crate::services::provides(loaded, key),
        "job" => crate::jobs::declared(loaded)
            .iter()
            .any(|job| &job.id == key),
        "rpc" | "command" => true,
        _ => false,
    };
    if !declared {
        return Err(RpcError::forbidden(format!(
            "{plugin} declares no {} {key}",
            registration.kind
        )));
    }
    if registration.methods.values().any(|method| {
        method
            .timeout_secs
            .is_some_and(|secs| !(1..=300).contains(&secs))
    }) {
        return Err(RpcError::invalid(
            "timeoutSecs must be an integer from 1 to 300",
        ));
    }
    Ok(())
}

/// Requests tied to this process and one live invocation stay on the private pipe.
async fn host_request(
    call: &Call,
    plugins: &Arc<crate::Plugins>,
    link: &Link,
    method: &str,
    params: Value,
) -> Result<Value, RpcError> {
    let text = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid(format!("{method} needs {key}")))
    };
    match method {
        "host.stream.item" | "host.job.log" | "host.job.progress" => {
            let invocation = link
                .0
                .invocations
                .lock()
                .unwrap()
                .get(text("callId")?)
                .cloned();
            match (method, invocation) {
                ("host.stream.item", Some(Invocation::Stream { items, cancelled })) => {
                    tokio::select! {
                        biased;
                        _ = cancelled.cancelled() => return Err(RpcError::unavailable("the service stream closed")),
                        _ = link.0.stop.cancelled() => return Err(RpcError::unavailable("the plugin stopped")),
                        sent = items.send(params.get("item").cloned().unwrap_or(Value::Null)) =>
                            sent.map_err(|_| RpcError::unavailable("the service stream closed"))?,
                    }
                }
                ("host.job.log", Some(Invocation::Job(run))) => {
                    let level = text("level")?;
                    if !matches!(level, "debug" | "info" | "warn" | "error") {
                        return Err(RpcError::invalid("a level is debug, info, warn or error"));
                    }
                    run.log(level, &link.redact(text("text")?), None).await?;
                }
                ("host.job.progress", Some(Invocation::Job(run))) => {
                    let mut progress = params.get("progress").cloned().unwrap_or(Value::Null);
                    link.redact_details(&mut progress);
                    run.progress(progress).await?;
                }
                _ => {
                    return Err(RpcError::forbidden(
                        "the process has no such running invocation",
                    ));
                }
            }
            Ok(Value::Null)
        }
        "jobs.log" | "jobs.progress" => Err(RpcError::forbidden(
            "a host half writes through its running job handler",
        )),
        "host.services.open" => {
            let capacity = Arc::new(reserve(&link.0.capacity.streams)?);
            let asked = serde_json::from_value(params)
                .map_err(|e| RpcError::invalid(format!("params: {e}")))?;
            let source =
                crate::services::open_stream(call, plugins, asked, Some(capacity.clone())).await?;
            let deadline = source.deadline();
            let id = uuid::Uuid::now_v7().to_string();
            let closed = CancellationToken::new();
            {
                let mut streams = link.0.streams.lock().unwrap();
                if !link.is_accepting() {
                    return Err(RpcError::unavailable("the plugin stopped"));
                }
                streams.insert(
                    id.clone(),
                    Arc::new(HostStream {
                        source: tokio::sync::Mutex::new(source),
                        closed: closed.clone(),
                        _capacity: capacity,
                    }),
                );
            }
            let (link, stream) = (link.clone(), id.clone());
            tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline.into()) => link.close_stream(&stream),
                    _ = closed.cancelled() => {}
                }
            });
            Ok(json!({"stream": id}))
        }
        "host.services.next" => {
            let id = text("stream")?;
            let stream = link
                .0
                .streams
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .ok_or_else(|| RpcError::not_found("no such service stream"))?;
            let mut source = stream
                .source
                .try_lock()
                .map_err(|_| RpcError::conflict("the service stream already has a reader"))?;
            let deadline = source.deadline();
            let next = tokio::select! {
                biased;
                _ = link.0.stop.cancelled() => Err(RpcError::unavailable("the plugin stopped")),
                _ = stream.closed.cancelled() => Err(
                    if Instant::now() >= deadline {
                        RpcError::timeout("the service stream timed out")
                    } else {
                        RpcError::unavailable("the service stream closed")
                    }
                ),
                next = source.next() => next,
            };
            if !matches!(next, Ok(Some(_))) {
                link.close_stream(id);
            }
            Ok(match next? {
                Some(item) => json!({"done": false, "item": item}),
                None => json!({"done": true}),
            })
        }
        "host.services.close" => {
            link.close_stream(text("stream")?);
            Ok(Value::Null)
        }
        _ => {
            call.kernel
                .methods
                .dispatch(call.clone(), method, params)
                .await
        }
    }
}
