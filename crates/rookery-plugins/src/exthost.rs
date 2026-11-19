//! The extension host: one Bun process the server starts when a plugin's host half is first needed,
//! with one worker in it for each plugin's host half. It runs `host/src/main.ts` of the source, and
//! the two talk JSON-RPC 2.0 over its standard input and output, one message a line.
//!
//! The server asks the host `plugins.activate`, `plugins.invoke` and `plugins.deactivate`, and tells
//! it `plugins.cancel` and `plugins.notify`, which carries what the kernel sent to the plugin's
//! connection, such as `events.event` and `topics.message`. The host asks `host.hello` first, then
//! the facility calls of plugins, each with the plugin in the envelope (`"plugin": "hello"`): they run
//! as that plugin, on a connection of its own. It tells the server `plugins.registered`,
//! `plugins.unregistered`, `plugins.exited` and `plugins.log`.
//!
//! A host that ends is started again, with a wait that grows while it keeps ending, and the plugins
//! that ran in it are activated again. A worker that ends is activated again the same way, up to
//! [`CRASHES`] times in [`CRASH_WINDOW`], after which its plugin is failed until it is activated by
//! hand or changed.

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
use tokio::sync::{mpsc, oneshot, watch};

use crate::wire::PluginState;

/// How a plugin's worker that keeps ending is given up on.
pub const CRASHES: usize = 5;
pub const CRASH_WINDOW: Duration = Duration::from_secs(60);
/// How long a new host may take to say hello, and a host half to activate.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(30);
const DEACTIVATE_TIMEOUT: Duration = Duration::from_secs(5);
/// The version of this protocol.
pub const PROTOCOL: &str = "1";

/// How the server starts the extension host.
#[derive(Debug, Clone)]
pub struct HostOptions {
    /// The Bun program.
    pub bun: PathBuf,
    /// `host/src/main.ts`. Without it no host half runs, and a plugin with one fails to activate.
    pub entry: Option<PathBuf>,
}

impl Default for HostOptions {
    fn default() -> Self {
        HostOptions {
            bun: PathBuf::from("bun"),
            entry: None,
        }
    }
}

/// Something a host half registered: a method of `rookery.rpc.handle`, a command and the like.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Registration {
    pub kind: String,
    pub key: String,
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
    /// The host it runs in.
    generation: u64,
    conn: Option<ConnHandle>,
    /// It is to run: it is activated again when its worker or the host ends.
    wanted: bool,
    crashes: Vec<Instant>,
}

pub(crate) struct ExtHost {
    options: HostOptions,
    link: watch::Sender<Option<Link>>,
    generation: AtomicU64,
    started: AtomicBool,
    slots: Mutex<HashMap<String, Slot>>,
    gates: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl ExtHost {
    pub fn new(options: HostOptions) -> ExtHost {
        ExtHost {
            options,
            link: watch::channel(None).0,
            generation: AtomicU64::new(0),
            started: AtomicBool::new(false),
            slots: Mutex::default(),
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

    /// The link of the running host.
    pub fn running(&self) -> Option<Link> {
        self.link.borrow().clone()
    }

    /// Serializes activating and deactivating one plugin.
    pub fn gate(&self, plugin: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.gates
            .lock()
            .unwrap()
            .entry(plugin.to_owned())
            .or_default()
            .clone()
    }

    /// The link of a plugin that runs in the current host.
    pub fn active_link(&self, plugin: &str) -> Option<Link> {
        let link = self.link.borrow().clone()?;
        let slots = self.slots.lock().unwrap();
        let slot = slots.get(plugin)?;
        (slot.state == Some(PluginState::Active) && slot.generation == link.generation())
            .then_some(link)
    }

    fn plugin_conn(&self, plugin: &str, generation: u64) -> Option<ConnHandle> {
        let slots = self.slots.lock().unwrap();
        let slot = slots.get(plugin)?;
        (slot.generation == generation)
            .then(|| slot.conn.clone())
            .flatten()
    }

    fn set(&self, plugin: &str, change: impl FnOnce(&mut Slot)) {
        let mut slots = self.slots.lock().unwrap();
        change(slots.entry(plugin.to_owned()).or_default());
    }

    /// Starts the host and its supervisor, once.
    fn ensure_started(
        &self,
        kernel: &Kernel,
        plugins: Arc<crate::Plugins>,
    ) -> Result<(), RpcError> {
        let Some(entry) = self.options.entry.clone() else {
            return Err(RpcError::unavailable(
                "no extension host is set up on this server (ROOKERY_EXTHOST), so no host half can run",
            ));
        };
        if !self.started.swap(true, Ordering::SeqCst) {
            tokio::spawn(supervise(kernel.clone(), plugins, entry));
        }
        Ok(())
    }

    async fn wait_link(&self, wait: Duration) -> Result<Link, RpcError> {
        let mut rx = self.link.subscribe();
        let ready = async {
            loop {
                if let Some(link) = rx.borrow_and_update().clone() {
                    return Some(link);
                }
                if rx.changed().await.is_err() {
                    return None;
                }
            }
        };
        match tokio::time::timeout(wait, ready).await {
            Ok(Some(link)) => Ok(link),
            _ => Err(RpcError::unavailable("the extension host is not running")),
        }
    }

    /// Starts a plugin's host half in the host, starting the host first when it does not run.
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
            self.ensure_started(kernel, plugins)?;
            let link = self.wait_link(CONNECT_TIMEOUT).await?;
            let (conn, rx) = ConnHandle::detached(kernel, Caller::Plugin(plugin.to_owned()));
            tokio::spawn(forward(rx, link.clone(), plugin.to_owned()));
            let params = json!({"plugin": plugin, "dir": dir, "manifest": manifest});
            match link
                .call::<Activated>("plugins.activate", params, ACTIVATE_TIMEOUT)
                .await
            {
                Ok(activated) => Ok((link, conn, activated.registrations)),
                Err(e) => {
                    conn.close_detached(kernel);
                    Err(e)
                }
            }
        }
        .await;
        match activated {
            Ok((link, conn, registrations)) => {
                tracing::info!(plugin, "a plugin's host half is active");
                self.set(plugin, |slot| {
                    slot.state = Some(PluginState::Active);
                    slot.generation = link.generation();
                    slot.conn = Some(conn);
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

    /// Stops a plugin's host half. One that is not wanted any more is not started again.
    pub async fn deactivate(&self, kernel: &Kernel, plugin: &str) {
        let gate = self.gate(plugin);
        let _one = gate.lock().await;
        let link = self.active_link(plugin);
        let conn = {
            let mut slots = self.slots.lock().unwrap();
            slots.remove(plugin).and_then(|slot| slot.conn)
        };
        if let Some(link) = link {
            let _ = link
                .call::<Value>(
                    "plugins.deactivate",
                    json!({"plugin": plugin}),
                    DEACTIVATE_TIMEOUT,
                )
                .await;
        }
        if let Some(conn) = conn {
            conn.close_detached(kernel);
        }
    }

    /// Runs one of a plugin's handlers in the host, as the caller of `call`.
    pub async fn invoke(
        &self,
        call: &Call,
        plugin: &str,
        kind: &str,
        key: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        let Some(link) = self.active_link(plugin) else {
            return Err(RpcError::unavailable(format!(
                "the host half of {plugin} does not run"
            )));
        };
        if !self
            .registrations(plugin)
            .iter()
            .any(|r| r.kind == kind && r.key == key)
        {
            return Err(RpcError::not_found(format!("{plugin} has no {kind} {key}")));
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
        match tokio::time::timeout(left, link.request("plugins.invoke", invoke)).await {
            Ok(done) => done,
            Err(_) => {
                link.notify(
                    "plugins.cancel",
                    json!({"plugin": plugin, "callId": call_id}),
                );
                Err(RpcError::timeout(format!(
                    "{plugin} did not answer {kind} {key} before the call's deadline"
                )))
            }
        }
    }

    /// The host connected: the plugins that ran in the one before run again.
    fn attached(&self, link: Link) -> Vec<String> {
        self.link.send_replace(Some(link));
        let slots = self.slots.lock().unwrap();
        slots
            .iter()
            .filter(|(_, slot)| slot.wanted)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The host went away: what ran in it does not run any more.
    fn detached(&self, kernel: &Kernel, generation: u64) {
        self.link.send_if_modified(|current| {
            if current
                .as_ref()
                .is_some_and(|l| l.generation() == generation)
            {
                *current = None;
                true
            } else {
                false
            }
        });
        let conns: Vec<ConnHandle> = {
            let mut slots = self.slots.lock().unwrap();
            slots
                .values_mut()
                .filter(|slot| slot.generation == generation)
                .filter_map(|slot| {
                    if slot.state == Some(PluginState::Active) {
                        slot.state = Some(PluginState::Inactive);
                    }
                    slot.registrations.clear();
                    slot.conn.take()
                })
                .collect()
        };
        for conn in conns {
            conn.close_detached(kernel);
        }
    }

    /// A plugin's worker ended without being asked to. Says how long to wait before it is activated
    /// again, or `None` when it ended too often.
    fn exited(&self, kernel: &Kernel, plugin: &str, error: Option<ErrorBody>) -> Option<Duration> {
        let now = Instant::now();
        let (conn, again) = {
            let mut slots = self.slots.lock().unwrap();
            let slot = slots.entry(plugin.to_owned()).or_default();
            slot.crashes
                .retain(|at| now.duration_since(*at) < CRASH_WINDOW);
            slot.crashes.push(now);
            slot.registrations.clear();
            let crashes = slot.crashes.len();
            let again = if slot.wanted && crashes < CRASHES {
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
                        "the host half of {plugin} ended {crashes} times in {} seconds, and is not started again until it is activated: {said}",
                        CRASH_WINDOW.as_secs()
                    ),
                    details: None,
                });
                None
            };
            (slot.conn.take(), again)
        };
        if let Some(conn) = conn {
            conn.close_detached(kernel);
        }
        again
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

#[derive(Deserialize)]
struct Activated {
    #[serde(default)]
    registrations: Vec<Registration>,
}

type Pending = Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>;

/// The server's side of the connection to one running host.
#[derive(Clone)]
pub struct Link(Arc<LinkInner>);

struct LinkInner {
    generation: u64,
    pid: Option<u32>,
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
    next: AtomicI64,
    closed: AtomicBool,
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

    /// A request. It answers `UNAVAILABLE` when the host goes away first, and has no time limit of
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

/// What the kernel sends a plugin's connection goes to its worker.
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

/// Starts the host and starts it again when it ends, until the server stops.
async fn supervise(kernel: Kernel, plugins: Arc<crate::Plugins>, entry: PathBuf) {
    let host = &plugins.host;
    let mut failures: u32 = 0;
    loop {
        let started = Instant::now();
        let generation = host.generation.fetch_add(1, Ordering::Relaxed) + 1;
        match spawn(&host.options.bun, &entry) {
            Ok(child) => {
                let done = run(&kernel, &plugins, child, generation);
                tokio::select! {
                    _ = done => {}
                    _ = kernel.shutdown.cancelled() => {}
                }
                host.detached(&kernel, generation);
                if kernel.shutdown.is_cancelled() {
                    return;
                }
            }
            Err(e) => {
                tracing::error!(error = %e, bun = %host.options.bun.display(), "the extension host could not start")
            }
        }
        failures = if started.elapsed() > Duration::from_secs(30) {
            0
        } else {
            failures + 1
        };
        let delay = Duration::from_millis(250 * 2u64.saturating_pow(failures.min(6)))
            .min(Duration::from_secs(10));
        tracing::info!(?delay, "the extension host starts again");
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = kernel.shutdown.cancelled() => return,
        }
    }
}

fn spawn(bun: &Path, entry: &Path) -> std::io::Result<Child> {
    // A plugin's folder has no node_modules of its own to find @rookery/sdk in: it gets the host
    // package's workspace's (host/src/main.ts → node_modules beside host/), which its workers inherit.
    let mut node_path: Vec<PathBuf> = std::env::var_os("NODE_PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(root) = std::path::absolute(entry)
        .ok()
        .and_then(|e| e.ancestors().nth(3).map(Path::to_path_buf))
    {
        node_path.push(root.join("node_modules"));
    }
    let mut cmd = Command::new(bun);
    cmd.arg(entry)
        .env(
            "NODE_PATH",
            std::env::join_paths(node_path).unwrap_or_default(),
        )
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

/// One host's life: its messages until it ends.
async fn run(kernel: &Kernel, plugins: &Arc<crate::Plugins>, mut child: Child, generation: u64) {
    let host = &plugins.host;
    let (out, mut rx) = mpsc::unbounded_channel::<String>();
    let link = Link(Arc::new(LinkInner {
        generation,
        pid: child.id(),
        out,
        pending: Mutex::default(),
        next: AtomicI64::new(1),
        closed: AtomicBool::new(false),
    }));
    tracing::info!(pid = ?child.id(), generation, "the extension host started");
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
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!(target: "rookery::exthost", "{line}");
            }
        });
    }
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut lines = BufReader::new(stdout).lines();
    let hello = tokio::time::sleep(CONNECT_TIMEOUT);
    tokio::pin!(hello);
    let mut said_hello = false;
    loop {
        let line = tokio::select! {
            line = lines.next_line() => line,
            _ = &mut hello, if !said_hello => {
                tracing::error!("the extension host did not say hello in time; it is started again");
                break;
            }
        };
        let Ok(Some(line)) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            tracing::warn!(target: "rookery::exthost", "{line}");
            continue;
        };
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").filter(|id| !id.is_null()).cloned();
        match (method, id) {
            (Some("host.hello"), Some(id)) => {
                link.respond(id, Ok(json!({"protocol": PROTOCOL})));
                if !said_hello {
                    said_hello = true;
                    for plugin in host.attached(link.clone()) {
                        let (kernel, plugins) = (kernel.clone(), plugins.clone());
                        tokio::spawn(async move {
                            let _ = plugins.activate(&kernel, &plugin).await;
                        });
                    }
                }
            }
            (Some(method), Some(id)) => {
                let plugin = message.get("plugin").and_then(Value::as_str).unwrap_or("");
                let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
                let Some(conn) = host.plugin_conn(plugin, generation) else {
                    link.respond(
                        id,
                        Err(RpcError::forbidden(format!(
                            "{plugin:?} is no plugin that runs in this host"
                        ))),
                    );
                    continue;
                };
                let call = Call::new(
                    kernel.clone(),
                    Caller::Plugin(plugin.to_owned()),
                    Some(conn),
                );
                let done = kernel.methods.dispatch(call, method, params);
                let link = link.clone();
                tokio::spawn(async move { link.respond(id, done.await) });
            }
            (Some(method), None) => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                notified(kernel, plugins, method, params);
            }
            (None, Some(_)) => link.resolve(&message),
            (None, None) => {}
        }
    }
    link.fail_all();
    writer.abort();
    let _ = child.start_kill();
    let status = child.wait().await;
    tracing::warn!(?status, generation, "the extension host ended");
}

fn notified(kernel: &Kernel, plugins: &Arc<crate::Plugins>, method: &str, params: Value) {
    let host = &plugins.host;
    let plugin = params
        .get("plugin")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    match method {
        "plugins.exited" => {
            let error = params
                .get("error")
                .and_then(|e| serde_json::from_value(e.clone()).ok());
            tracing::warn!(plugin, ?error, "a plugin's host half ended");
            let again = host.exited(kernel, &plugin, error);
            plugins.changed(kernel, std::slice::from_ref(&plugin));
            if let Some(wait) = again {
                let (kernel, plugins) = (kernel.clone(), plugins.clone());
                tokio::spawn(async move {
                    tokio::time::sleep(wait).await;
                    let _ = plugins.activate(&kernel, &plugin).await;
                });
            }
        }
        "plugins.registered" => {
            if let Some(registration) = params
                .get("registration")
                .and_then(|r| serde_json::from_value(r.clone()).ok())
            {
                host.registered(&plugin, registration);
            }
        }
        "plugins.unregistered" => {
            let text = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or("");
            host.unregistered(&plugin, text("kind"), text("key"));
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
