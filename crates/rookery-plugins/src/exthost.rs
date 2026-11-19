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

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rookery_kernel::conn::{ConnHandle, Outbound};
use rookery_kernel::{Call, Caller, Kernel};
use rookery_proto::{ErrorBody, ErrorCode, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

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

pub(crate) struct ExtHost {
    options: HostOptions,
    generation: AtomicU64,
    slots: Mutex<HashMap<String, Slot>>,
    procs: Mutex<HashMap<String, Proc>>,
    gates: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl ExtHost {
    pub fn new(options: HostOptions) -> ExtHost {
        ExtHost {
            options,
            generation: AtomicU64::new(0),
            slots: Mutex::default(),
            procs: Mutex::default(),
            gates: Mutex::default(),
        }
    }

    /// The state of a plugin's host half, and why it failed; `None` while it never ran.
    pub fn state(&self, plugin: &str) -> (Option<PluginState>, Option<ErrorBody>) {
        let slots = self.slots.lock().unwrap();
        match slots.get(plugin) {
            Some(slot) => (slot.state, slot.error.clone()),
            None => (None, None),
        }
    }

    pub fn registrations(&self, plugin: &str) -> Vec<Registration> {
        let slots = self.slots.lock().unwrap();
        slots
            .get(plugin)
            .map(|slot| slot.registrations.clone())
            .unwrap_or_default()
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
        let link = self.running(plugin)?;
        let slots = self.slots.lock().unwrap();
        let slot = slots.get(plugin)?;
        (slot.state == Some(PluginState::Active) && slot.generation == link.generation())
            .then_some(link)
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
        if let Some(link) = self.running(plugin).filter(|l| !l.is_closed()) {
            return Ok(link);
        }
        let Some(entry) = self.options.entry.clone() else {
            return Err(RpcError::unavailable(
                "no extension host is set up on this server (ROOKERY_EXTHOST), so no host half can run",
            ));
        };
        let child = spawn(&self.options, &entry).map_err(|e| {
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
            next: AtomicI64::new(1),
            closed: AtomicBool::new(false),
            stop: CancellationToken::new(),
        }));
        let (conn, notices) = ConnHandle::detached(kernel, Caller::Plugin(plugin.to_owned()));
        tokio::spawn(forward(notices, link.clone(), plugin.to_owned()));
        self.procs.lock().unwrap().insert(
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
        match tokio::time::timeout(CONNECT_TIMEOUT, said_hello).await {
            Ok(Ok(())) => Ok(link),
            _ => {
                self.stop(kernel, plugin, generation);
                Err(RpcError::unavailable(format!(
                    "the extension host of {plugin} did not start"
                )))
            }
        }
    }

    /// Ends a plugin's process on purpose: what it was doing does not count as a crash.
    fn stop(&self, kernel: &Kernel, plugin: &str, generation: u64) {
        let proc = {
            let mut procs = self.procs.lock().unwrap();
            match procs.get(plugin) {
                Some(p) if p.link.generation() == generation => procs.remove(plugin),
                _ => None,
            }
        };
        if let Some(proc) = proc {
            proc.link.stop();
            proc.conn.close_detached(kernel);
            // What was active in it is not any more; it starts again when it is next used.
            let mut slots = self.slots.lock().unwrap();
            if let Some(slot) = slots.get_mut(plugin)
                && slot.generation == generation
                && slot.state == Some(PluginState::Active)
            {
                slot.state = Some(PluginState::Inactive);
                slot.registrations.clear();
            }
        }
    }

    /// Deactivates every host half, which runs its cleanups, within `limit` in all, and ends the
    /// processes. The server calls it when it stops, before it closes the store.
    pub async fn stop_all(&self, kernel: &Kernel, limit: Duration) {
        let plugins: Vec<String> = self.procs.lock().unwrap().keys().cloned().collect();
        let all = futures_util::future::join_all(
            plugins.iter().map(|plugin| self.deactivate(kernel, plugin)),
        );
        if tokio::time::timeout(limit, all).await.is_err() {
            tracing::warn!("host halves did not finish their cleanups in time");
        }
        let left: Vec<(String, u64)> = self
            .procs
            .lock()
            .unwrap()
            .iter()
            .map(|(id, p)| (id.clone(), p.link.generation()))
            .collect();
        for (plugin, generation) in left {
            self.stop(kernel, &plugin, generation);
        }
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
                    self.stop(kernel, plugin, link.generation());
                }
                done.map(|_| ())
            }
            Err(_) => {
                // Its worker is still running: the process goes, and with it the step.
                self.stop(kernel, plugin, link.generation());
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
        plugin: &str,
        dir: &Path,
        manifest: &Value,
    ) -> Result<(), RpcError> {
        let gate = self.gate(plugin);
        let _one = gate.lock().await;
        if self.active_link(plugin).is_some() {
            return Ok(());
        }
        self.set(plugin, |slot| {
            slot.state = Some(PluginState::Activating);
            slot.error = None;
            slot.wanted = true;
        });
        let activated = async {
            let link = self.start(kernel, &plugins, plugin).await?;
            let params = json!({"plugin": plugin, "dir": dir, "manifest": manifest});
            match link
                .call::<Activated>("plugins.activate", params, ACTIVATE_TIMEOUT)
                .await
            {
                Ok(activated) => Ok((link, activated.registrations)),
                Err(e) => {
                    self.stop(kernel, plugin, link.generation());
                    Err(e)
                }
            }
        }
        .await;
        match activated {
            Ok((link, registrations)) => {
                tracing::info!(plugin, pid = ?link.pid(), "a plugin's host half is active");
                self.set(plugin, |slot| {
                    slot.state = Some(PluginState::Active);
                    slot.generation = link.generation();
                    slot.registrations = registrations;
                });
                Ok(())
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
    pub async fn deactivate(&self, kernel: &Kernel, plugin: &str) {
        let gate = self.gate(plugin);
        let _one = gate.lock().await;
        self.slots.lock().unwrap().remove(plugin);
        let Some(link) = self.running(plugin) else {
            return;
        };
        let _ = link
            .call::<Value>(
                "plugins.deactivate",
                json!({"plugin": plugin}),
                DEACTIVATE_TIMEOUT,
            )
            .await;
        self.stop(kernel, plugin, link.generation());
    }

    /// Runs one of a plugin's handlers, as the caller of `call`. An `rpc` handler says who may call it.
    /// A handler that does not stop within [`CANCEL_GRACE`] of its deadline ends its host half.
    pub async fn invoke(
        &self,
        call: &Call,
        plugins: &Arc<crate::Plugins>,
        plugin: &str,
        kind: &str,
        key: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        self.invoke_until(call, plugins, plugin, kind, key, params, None)
            .await
    }

    /// [`ExtHost::invoke`], which `cancel` also ends, as a job's cancel does. Only an `rpc` handler
    /// says a level: the server itself calls the others, such as a job's.
    #[allow(clippy::too_many_arguments)]
    pub async fn invoke_until(
        &self,
        call: &Call,
        plugins: &Arc<crate::Plugins>,
        plugin: &str,
        kind: &str,
        key: &str,
        params: Value,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<Value, RpcError> {
        let Some(link) = self.active_link(plugin) else {
            return Err(RpcError::unavailable(format!(
                "the host half of {plugin} does not run"
            )));
        };
        let Some(registration) = self
            .registrations(plugin)
            .into_iter()
            .find(|r| r.kind == kind && r.key == key)
        else {
            return Err(RpcError::not_found(format!("{plugin} has no {kind} {key}")));
        };
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
        let deadline = chrono::Utc::now().timestamp_millis()
            + i64::try_from(left.as_millis()).unwrap_or(i64::MAX / 2);
        let ctx = json!({
            "callId": call_id,
            "user": call.user(),
            "principal": call.principal(),
            "deadline": deadline,
        });
        let invoke =
            json!({"plugin": plugin, "kind": kind, "key": key, "params": params, "ctx": ctx});
        let mut request = Box::pin(link.request("plugins.invoke", invoke));
        let cancelled = async {
            match &cancel {
                Some(cancel) => cancel.cancelled().await,
                None => std::future::pending().await,
            }
        };
        let reason = tokio::select! {
            done = &mut request => return done,
            _ = tokio::time::sleep(left) => "timeout",
            _ = cancelled => "cancelled",
        };
        link.notify(
            "plugins.cancel",
            json!({"plugin": plugin, "callId": call_id, "reason": reason}),
        );
        let (kernel, plugins, id) = (call.kernel.clone(), plugins.clone(), plugin.to_owned());
        let generation = link.generation();
        tokio::spawn(async move {
            if tokio::time::timeout(CANCEL_GRACE, request).await.is_err() {
                let error = ErrorBody {
                    code: ErrorCode::Timeout,
                    message: format!("{id} did not stop a call {CANCEL_GRACE:?} past its deadline"),
                    details: None,
                };
                crashed(&kernel, &plugins, &id, generation, Some(error));
            }
        });
        match reason {
            "cancelled" => Err(RpcError::unavailable(format!(
                "{kind} {key} of {plugin} was cancelled"
            ))),
            _ => Err(RpcError::timeout(format!(
                "{plugin} did not answer {kind} {key} before the call's deadline"
            ))),
        }
    }

    /// Counts a crash of a plugin's host half. Says how long to wait before it is activated again, or
    /// `None` when it ended too often or is not wanted.
    fn exited(&self, plugin: &str, error: Option<ErrorBody>) -> Option<Duration> {
        let now = Instant::now();
        let mut slots = self.slots.lock().unwrap();
        let slot = slots.get_mut(plugin)?;
        slot.crashes
            .retain(|at| now.duration_since(*at) < CRASH_WINDOW);
        slot.crashes.push(now);
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

/// A plugin's host half ended, or is ended, without being asked to: its process goes, the crash is
/// counted against the plugin, and it is activated again when it may be.
fn crashed(
    kernel: &Kernel,
    plugins: &Arc<crate::Plugins>,
    plugin: &str,
    generation: u64,
    error: Option<ErrorBody>,
) {
    let host = &plugins.host;
    let current = host
        .running(plugin)
        .is_some_and(|l| l.generation() == generation);
    if !current || kernel.shutdown.is_cancelled() {
        return;
    }
    host.stop(kernel, plugin, generation);
    tracing::warn!(plugin, ?error, "a plugin's host half ended");
    let again = host.exited(plugin, error);
    plugins.changed(kernel, &[plugin.to_owned()]);
    if let Some(wait) = again {
        let (kernel, plugins, plugin) = (kernel.clone(), plugins.clone(), plugin.to_owned());
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            let _ = plugins.activate(&kernel, &plugin).await;
        });
    }
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
    next: AtomicI64,
    closed: AtomicBool,
    /// Ends the process.
    stop: CancellationToken,
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

    /// A request. It answers `UNAVAILABLE` when the process goes away first, and has no time limit of
    /// its own.
    pub fn request(
        &self,
        method: &str,
        params: Value,
    ) -> impl Future<Output = Result<Value, RpcError>> + Send + 'static {
        let (tx, rx) = oneshot::channel();
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        let sent = !self.0.closed.load(Ordering::Acquire) && {
            self.0.pending.lock().unwrap().insert(id, tx);
            self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
        };
        if !sent {
            self.0.pending.lock().unwrap().remove(&id);
        }
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
            Some(error) => Err(serde_json::from_value::<RpcError>(error.clone())
                .unwrap_or_else(|_| RpcError::internal(error.to_string()))),
            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = tx.send(result);
    }

    fn fail_all(&self) {
        self.0.closed.store(true, Ordering::Release);
        let pending: Vec<_> = self.0.pending.lock().unwrap().drain().collect();
        for (_, tx) in pending {
            let _ = tx.send(Err(RpcError::unavailable("the extension host went away")));
        }
    }

    fn is_closed(&self) -> bool {
        self.0.closed.load(Ordering::Acquire)
    }
}

/// What the kernel sends a plugin's connection goes to its process.
async fn forward(mut rx: mpsc::Receiver<Outbound>, link: Link, plugin: String) {
    while let Some(out) = rx.recv().await {
        if link.is_closed() {
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
    if let Some(err) = child.stderr.take() {
        let plugin = plugin.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!(target: "rookery::exthost", plugin, "{line}");
            }
        });
    }
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
            line = lines.next_line() => line,
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
        };
        let Ok(Some(line)) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            tracing::warn!(target: "rookery::exthost", plugin, "{line}");
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
                let done = kernel.methods.dispatch(call, method, params);
                let link = link.clone();
                tokio::spawn(async move { link.respond(id, done.await) });
            }
            (Some(method), None) => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                notified(&kernel, &plugins, &plugin, generation, method, params);
            }
            (None, Some(_)) => link.resolve(&message),
            (None, None) => {}
        }
    }
    link.fail_all();
    writer.abort();
    let _ = child.start_kill();
    let status = child.wait().await;
    tracing::info!(
        plugin,
        ?status,
        generation,
        "a plugin's extension host ended"
    );
    let error = over.unwrap_or_else(|| ErrorBody {
        code: ErrorCode::Unavailable,
        message: format!("the extension host of {plugin} ended ({status:?})"),
        details: None,
    });
    crashed(&kernel, &plugins, &plugin, generation, Some(error));
}

/// A notification of a plugin's process, about that plugin alone.
fn notified(
    kernel: &Kernel,
    plugins: &Arc<crate::Plugins>,
    plugin: &str,
    generation: u64,
    method: &str,
    params: Value,
) {
    let host = &plugins.host;
    match method {
        "plugins.exited" => {
            let error = params
                .get("error")
                .and_then(|e| serde_json::from_value(e.clone()).ok());
            crashed(kernel, plugins, plugin, generation, error);
        }
        "plugins.registered" => {
            if let Some(registration) = params
                .get("registration")
                .and_then(|r| serde_json::from_value(r.clone()).ok())
            {
                host.registered(plugin, registration);
            }
        }
        "plugins.unregistered" => {
            let text = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or("");
            host.unregistered(plugin, text("kind"), text("key"));
        }
        "plugins.log" => {
            let message = params.get("message").and_then(Value::as_str).unwrap_or("");
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
