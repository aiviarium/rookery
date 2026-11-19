//! Terminals: `terminals.*`. A terminal is a session on a host with a record in `rookery.terminals`:
//! whose it is, its size and how far its output came. The host keeps the screen and the output; the
//! server passes bytes through and keeps who watches, the size rule and the offsets.
//!
//! A terminal is its owner's. Its level is `use` for its owner while they hold `use` on the host,
//! and `manage` for whoever manages the host. Nobody else sees it, whatever they hold on the host.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use rookery_api::client::Frame;
use rookery_core::host_type::Capability;
use rookery_kernel::streams::{Inbound, Initiator};
use rookery_kernel::{Call, Empty, Grant, Kernel, Level, Methods, RpcError};
use rookery_proto::terminals::*;
use rookery_store::terminals::{New, Query, Terminal};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::serve::api;

/// The resource a terminal is, for levels.
pub fn uri(id: &str) -> String {
    format!("terminal://{id}")
}

/// A level on the terminal the param `id` names.
fn level(grant: Grant) -> Level {
    Level::Resource {
        param: "id".into(),
        grant,
        scheme: Some("terminal".into()),
    }
}

/// The published event when a terminal changes.
pub const TERMINALS_CHANGED: &str = "rookery/terminals.changed";

/// An attach that would replay more than this starts with the screen instead.
const RESUME_AT_MOST: u64 = 1 << 20;

/// The default command: the agent's login shell.
const SHELL: &str = "exec \"${SHELL:-/bin/bash}\" -l";

/// The caller's level on a terminal: manage on its host, or use on it for its owner.
pub async fn level_of(call: &Call, terminal: &Terminal) -> Result<Option<Grant>, RpcError> {
    if call.is_admin() {
        return Ok(Some(Grant::Manage));
    }
    let host = rookery_facilities::access::level(call, &crate::uri(&terminal.host)).await?;
    let own = match (&call.caller, terminal.owner) {
        (rookery_kernel::Caller::User { user, .. }, Some(owner)) => user.id == owner.to_string(),
        _ => false,
    };
    Ok(match host {
        Some(Grant::Manage) => Some(Grant::Manage),
        Some(Grant::Use) if own => Some(Grant::Use),
        _ => None,
    })
}

/// The check behind `Level::Resource` for `terminal://ID`, in front of the one for every other
/// resource.
pub fn wrap_access(methods: &mut Methods) {
    let inner = methods.access();
    methods.set_access(move |call, resource, wanted| {
        let inner = inner.clone();
        async move {
            if let Some(id) = resource.strip_prefix("terminal://") {
                let Some(terminal) = call.store().terminals().get(id).await? else {
                    return Err(RpcError::not_found(format!("no terminal {id}")));
                };
                return Ok(level_of(&call, &terminal)
                    .await?
                    .is_some_and(|held| held >= wanted));
            }
            match inner {
                Some(check) => check(call, resource, wanted).await,
                None => Ok(false),
            }
        }
    });
}

// ── Live state ──────────────────────────────────────────────────────────────

/// The viewers of a terminal while any are attached.
struct Live {
    events: broadcast::Sender<TerminalEvent>,
    state: Mutex<LiveState>,
}

struct LiveState {
    viewers: BTreeMap<String, Viewer>,
    /// The viewer whose size the terminal takes.
    active: Option<String>,
    size: TermSize,
    offset: u64,
}

struct Viewer {
    size: Option<TermSize>,
    readonly: bool,
}

/// The terminals attached now, on the leader.
#[derive(Default)]
pub struct Hub {
    live: Mutex<HashMap<String, Arc<Live>>>,
    next: std::sync::atomic::AtomicU64,
}

impl Hub {
    fn of(kernel: &Kernel) -> Arc<Hub> {
        kernel.facility::<Hub>()
    }

    fn join(
        &self,
        terminal: &Terminal,
        size: Option<TermSize>,
        readonly: bool,
    ) -> (Arc<Live>, String) {
        let n = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let viewer = format!("v{n}");
        let mut all = self.live.lock().unwrap();
        let live = all
            .entry(terminal.id.clone())
            .or_insert_with(|| {
                Arc::new(Live {
                    events: broadcast::channel(64).0,
                    state: Mutex::new(LiveState {
                        viewers: BTreeMap::new(),
                        active: None,
                        size: TermSize {
                            cols: terminal.cols as u16,
                            rows: terminal.rows as u16,
                        },
                        offset: terminal.offset as u64,
                    }),
                })
            })
            .clone();
        live.state
            .lock()
            .unwrap()
            .viewers
            .insert(viewer.clone(), Viewer { size, readonly });
        (live, viewer)
    }

    fn leave(&self, id: &str, live: &Arc<Live>, viewer: &str) -> (u32, Option<TermSize>) {
        let mut state = live.state.lock().unwrap();
        state.viewers.remove(viewer);
        let mut size = None;
        if state.active.as_deref() == Some(viewer) {
            // The size goes to the viewer attached last that can type.
            state.active = state
                .viewers
                .iter()
                .rev()
                .find(|(_, v)| !v.readonly && v.size.is_some())
                .map(|(id, _)| id.clone());
            size = state
                .active
                .as_ref()
                .and_then(|a| state.viewers.get(a))
                .and_then(|v| v.size.clone());
        }
        let count = state.viewers.len() as u32;
        if count == 0 {
            self.live.lock().unwrap().remove(id);
        }
        (count, size)
    }

    fn viewers(&self, id: &str) -> u32 {
        self.live
            .lock()
            .unwrap()
            .get(id)
            .map_or(0, |live| live.state.lock().unwrap().viewers.len() as u32)
    }

    fn offset(&self, id: &str) -> Option<u64> {
        self.live
            .lock()
            .unwrap()
            .get(id)
            .map(|live| live.state.lock().unwrap().offset)
    }

    fn tell(&self, id: &str, event: TerminalEvent) {
        if let Some(live) = self.live.lock().unwrap().get(id) {
            let _ = live.events.send(event);
        }
    }
}

// ── Records ─────────────────────────────────────────────────────────────────

fn state_of(state: &str) -> TerminalState {
    match state {
        "exited" => TerminalState::Exited,
        "lost" => TerminalState::Lost,
        _ => TerminalState::Running,
    }
}

fn user_of(run_as: &str) -> EnvUser {
    match run_as {
        "operator" => EnvUser::Operator,
        "root" => EnvUser::Root,
        _ => EnvUser::Agent,
    }
}

/// A record as the wire has it, for a caller at `level`.
pub fn info(kernel: &Kernel, t: &Terminal, level: Option<Grant>) -> TerminalInfo {
    let hub = Hub::of(kernel);
    TerminalInfo {
        id: t.id.clone(),
        uri: uri(&t.id),
        env: crate::uri(&t.host),
        title: t.title.clone(),
        command: serde_json::from_value(t.command.clone()).unwrap_or_default(),
        cwd: t.cwd.clone(),
        owner: t.owner.map(|u| u.to_string()),
        user: user_of(&t.run_as),
        state: state_of(&t.state),
        exit_code: t.exit_code,
        size: TermSize {
            cols: t.cols as u16,
            rows: t.rows as u16,
        },
        viewers: hub.viewers(&t.id),
        offset: hub
            .offset(&t.id)
            .unwrap_or(t.offset as u64)
            .max(t.offset as u64),
        created_at: t.created_at.to_rfc3339(),
        session_id: t.session.clone(),
        recording: false,
        level,
        last_input: t
            .last_input
            .clone()
            .and_then(|v| serde_json::from_value(v).ok()),
    }
}

async fn record(call: &Call, id: &str) -> Result<Terminal, RpcError> {
    call.store()
        .terminals()
        .get(id)
        .await?
        .ok_or_else(|| RpcError::not_found(format!("no terminal {id}")))
}

/// The link of a terminal's host.
async fn link(call: &Call, host: &str) -> Result<Arc<crate::serve::Remote>, RpcError> {
    let state = crate::env::serve(call)?;
    Ok(api::controller(&state, host, Capability::Sessions).await?)
}

fn session_path(session: &str, rest: &str) -> String {
    format!(
        "/v1/sessions/{}{rest}",
        rookery_api::client::encode(session)
    )
}

/// Asks the host for a session's JSON.
async fn ask(
    remote: &crate::serve::Remote,
    method: &str,
    target: &str,
    body: Option<&Value>,
) -> Result<Value, RpcError> {
    Ok(api::pass(remote, method, target, body).await?)
}

/// Tells those who may see a terminal that it changed, each with their level.
pub async fn changed(kernel: &Kernel, terminal: &Terminal) {
    let kernel2 = kernel.clone();
    let terminal = terminal.clone();
    rookery_jobs::events::publish_each(kernel, TERMINALS_CHANGED, move |caller| {
        let kernel = kernel2.clone();
        let terminal = terminal.clone();
        async move {
            let call = Call::new(kernel.clone(), caller, None);
            let level = level_of(&call, &terminal).await.ok().flatten()?;
            serde_json::to_value(TerminalChanged {
                terminal: info(&kernel, &terminal, Some(level)),
            })
            .ok()
        }
    })
    .await;
}

/// Brings a running record up to date with the host's session: ended, or gone with the host's
/// controller. A host that does not answer leaves it as it is.
async fn reconcile(call: &Call, terminals: &mut [Terminal]) {
    let mut by_host: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, t) in terminals.iter().enumerate() {
        if t.state == "running" {
            by_host.entry(t.host.clone()).or_default().push(i);
        }
    }
    for (host, indexes) in by_host {
        let Ok(remote) = link(call, &host).await else {
            continue;
        };
        let Ok(Value::Array(sessions)) = ask(&remote, "GET", "/v1/sessions", None).await else {
            continue;
        };
        let sessions: HashMap<&str, &Value> = sessions
            .iter()
            .filter_map(|s| Some((s["id"].as_str()?, s)))
            .collect();
        for i in indexes {
            let t = &mut terminals[i];
            let (state, exit) = match sessions.get(t.session.as_str()) {
                None => ("lost", None),
                Some(s) if s["ended"].is_string() => (
                    if s["lost"] == true { "lost" } else { "exited" },
                    s["exit_code"].as_i64().map(|c| c as i32),
                ),
                Some(_) => continue,
            };
            if call
                .store()
                .terminals()
                .end(&t.id, state, exit)
                .await
                .unwrap_or(false)
            {
                t.state = state.to_owned();
                t.exit_code = exit;
                changed(&call.kernel, t).await;
            }
        }
    }
}

// ── Methods ─────────────────────────────────────────────────────────────────

pub fn register(methods: &mut Methods) {
    methods
        .add("terminals.open", |call: Call, p: TerminalOpen| async move {
            let name = crate::name_of(&p.env).to_owned();
            let env = crate::uri(&name);
            if p.user != EnvUser::Agent
                && !rookery_facilities::access::holds(&call, &env, Grant::Manage).await?
            {
                return Err(RpcError::forbidden(format!(
                    "a terminal as {} needs manage on {env}",
                    p.user.as_str()
                )));
            }
            let remote = link(&call, &name).await?;
            let size = p.size.clone().unwrap_or(TermSize { cols: 80, rows: 24 });
            let command = match p.command.is_empty() {
                true => rookery_api::CommandLine::Shell(SHELL.into()),
                false => rookery_api::CommandLine::Args(p.command.clone()),
            };
            let title = p.title.clone().unwrap_or_else(|| match p.command.is_empty() {
                true => "shell".into(),
                false => p.command.join(" "),
            });
            let start = rookery_api::StartSession {
                kind: rookery_api::SessionKind::Terminal,
                command,
                user: p.user.as_str().into(),
                cwd: p.cwd.clone(),
                env: p.vars.clone(),
                input: None,
                input_b64: None,
                title: Some(title.clone()),
                timeout: None,
                rows: Some(size.rows),
                cols: Some(size.cols),
            };
            let body = serde_json::to_value(&start).map_err(|e| RpcError::internal(e.to_string()))?;
            let started = ask(&remote, "POST", "/v1/sessions", Some(&body)).await?;
            let session = started["id"]
                .as_str()
                .ok_or_else(|| RpcError::internal("the host named no session"))?;
            let id = new_id();
            let command_words = match p.command.is_empty() {
                true => json!([]),
                false => json!(p.command),
            };
            let terminal = call
                .store()
                .terminals()
                .insert(New {
                    id: &id,
                    host: &name,
                    session,
                    owner: call.user_id().ok(),
                    run_as: p.user.as_str(),
                    title: &title,
                    command: &command_words,
                    cwd: p.cwd.as_deref(),
                    cols: size.cols as i32,
                    rows: size.rows as i32,
                })
                .await?;
            call.audit("terminal.open", Some(&env), json!({"terminal": id, "user": p.user.as_str()}))
                .await;
            changed(&call.kernel, &terminal).await;
            let level = level_of(&call, &terminal).await?;
            Ok(info(&call.kernel, &terminal, level))
        })
        .summary("Opens a terminal on a host, as the agent account with use, or as operator or root with manage. It is the caller's own.")
        .level(crate::env::level(Grant::Use));
    methods
        .add("terminals.get", |call: Call, p: TerminalRef| async move {
            let mut found = [record(&call, &p.id).await?];
            reconcile(&call, &mut found).await;
            let level = level_of(&call, &found[0]).await?;
            Ok(info(&call.kernel, &found[0], level))
        })
        .summary("One terminal: the caller's own, or one on a host they manage.")
        .level(level(Grant::View));
    methods
        .add("terminals.list", |call: Call, p: TerminalQuery| async move {
            let host = p.env.as_deref().map(crate::name_of);
            let owner = match (p.all, call.user_id()) {
                (false, Ok(user)) => Some(user),
                (false, Err(_)) if !call.is_admin() => return Ok(TerminalList { terminals: vec![] }),
                _ => None,
            };
            let mut found = call
                .store()
                .terminals()
                .list(Query { host, owner, ended: p.ended })
                .await?;
            let mut terminals = vec![];
            let mut seen = vec![];
            for t in found.drain(..) {
                if let Some(level) = level_of(&call, &t).await? {
                    seen.push((t, level));
                }
            }
            let (mut records, levels): (Vec<_>, Vec<_>) = seen.into_iter().unzip();
            reconcile(&call, &mut records).await;
            for (t, level) in records.iter().zip(levels) {
                if p.ended || t.state == "running" {
                    terminals.push(info(&call.kernel, t, Some(level)));
                }
            }
            Ok(TerminalList { terminals })
        })
        .summary("The caller's terminals, running first and newest first, with ended the last 50 that ended; with all, also others' on hosts the caller manages.")
        .level(Level::Own);
    methods
        .add("terminals.attach", |call: Call, p: TerminalAttach| async move {
            attach(call, p).await
        })
        .summary("A stream of a terminal: its screen or the output after an offset, then output as it comes, events of its size, viewers and state, and the viewer's input. Frames carry a type byte: 0 output or input, 1 an event or a control in JSON, 2 the screen.")
        .stream()
        .level(level(Grant::Use));
    methods
        .add(
            "terminals.write",
            |call: Call, p: TerminalWrite| async move {
                let t = record(&call, &p.id).await?;
                let bytes = match (&p.base64, &p.text) {
                    (Some(b), _) => rookery_api::unb64(b)
                        .ok_or_else(|| RpcError::invalid("base64: not base64"))?,
                    (None, Some(text)) => text.clone().into_bytes(),
                    (None, None) => return Ok(Empty {}),
                };
                let remote = link(&call, &t.host).await?;
                ask(
                    &remote,
                    "POST",
                    &session_path(&t.session, "/input"),
                    Some(&json!({"data_b64": rookery_api::b64(&bytes)})),
                )
                .await?;
                typed(&call, &t.id, "automation", bytes.len() as u64).await;
                Ok(Empty {})
            },
        )
        .summary("Types into a terminal, as text or bytes in base64, for automation.")
        .level(level(Grant::Use));
    methods
        .add(
            "terminals.resize",
            |call: Call, p: TerminalResize| async move {
                let t = record(&call, &p.id).await?;
                resize(&call.kernel, &call, &t, p.size, None).await?;
                Ok(Empty {})
            },
        )
        .summary("Gives a terminal a size, as an active viewer would.")
        .level(level(Grant::Use));
    methods
        .add(
            "terminals.signal",
            |call: Call, p: TerminalSignal| async move {
                let signal = p.signal.trim_start_matches("SIG").to_ascii_uppercase();
                if !["INT", "TERM", "HUP", "KILL"].contains(&signal.as_str()) {
                    return Err(RpcError::invalid("signal: INT, TERM, HUP or KILL"));
                }
                let t = record(&call, &p.id).await?;
                let remote = link(&call, &t.host).await?;
                ask(
                    &remote,
                    "POST",
                    &session_path(&t.session, "/signal"),
                    Some(&json!({"signal": signal})),
                )
                .await?;
                Ok(Empty {})
            },
        )
        .summary("Sends a signal to a terminal's process: INT, TERM, HUP or KILL.")
        .level(level(Grant::Use));
    methods
        .add(
            "terminals.rename",
            |call: Call, p: TerminalRename| async move {
                let title = p.title.trim();
                if title.is_empty() || title.len() > 200 {
                    return Err(RpcError::invalid("title: 1 to 200 characters"));
                }
                call.store().terminals().set_title(&p.id, title).await?;
                let t = record(&call, &p.id).await?;
                Hub::of(&call.kernel).tell(
                    &t.id,
                    TerminalEvent::Title {
                        title: title.to_owned(),
                    },
                );
                changed(&call.kernel, &t).await;
                Ok(Empty {})
            },
        )
        .summary("Gives a terminal another title.")
        .level(level(Grant::Use));
    methods
        .add("terminals.close", |call: Call, p: TerminalRef| async move {
            let t = record(&call, &p.id).await?;
            // The host forgets its session either way; one that ran ends first.
            if let Ok(remote) = link(&call, &t.host).await {
                match ask(&remote, "DELETE", &session_path(&t.session, ""), None).await {
                    Ok(_) => {}
                    Err(e) if e.error_code() == rookery_proto::ErrorCode::NotFound => {}
                    Err(e) if t.state == "running" => return Err(e),
                    Err(_) => {}
                }
            } else if t.state == "running" {
                link(&call, &t.host).await?;
            }
            let store = call.store().terminals();
            if t.state == "running" {
                store.end(&t.id, "exited", None).await?;
            }
            store.delete(&t.id).await?;
            Hub::of(&call.kernel).tell(
                &t.id,
                TerminalEvent::State {
                    state: TerminalState::Exited,
                    exit_code: None,
                },
            );
            let mut ended = t.clone();
            ended.state = "exited".into();
            changed(&call.kernel, &ended).await;
            call.audit(
                "terminal.close",
                Some(&crate::uri(&t.host)),
                json!({"terminal": t.id}),
            )
            .await;
            Ok(Empty {})
        })
        .summary("Ends a terminal, and forgets it and its session on the host.")
        .level(level(Grant::Use));
    methods
        .add("terminals.screen", |call: Call, p: ScreenQuery| async move {
            let t = record(&call, &p.id).await?;
            let remote = link(&call, &t.host).await?;
            let text = ask(&remote, "GET", &session_path(&t.session, "/screen"), None).await?;
            let screen: rookery_api::Screen =
                serde_json::from_value(text).map_err(|e| RpcError::internal(e.to_string()))?;
            let lines = match p.history {
                Some(history) => {
                    let tail = history as usize + screen.rows as usize;
                    let all = ask(&remote, "GET", &session_path(&t.session, &format!("/screen?tail={tail}")), None).await?;
                    all["text"].as_str().unwrap_or_default().split('\n').map(str::to_owned).collect()
                }
                None => screen.text.split('\n').map(|l| l.trim_end().to_owned()).collect(),
            };
            let ansi = match p.ansi {
                true => ask(&remote, "GET", &session_path(&t.session, "/screen?format=ansi"), None)
                    .await?["text"]
                    .as_str()
                    .map(str::to_owned),
                false => None,
            };
            Ok(ScreenInfo {
                size: TermSize { cols: screen.cols, rows: screen.rows },
                lines,
                cursor: CursorInfo { row: screen.cursor_row, col: screen.cursor_col },
                offset: info(&call.kernel, &t, None).offset,
                ansi,
            })
        })
        .summary("A terminal's screen as the host keeps it: its lines and cursor, with history the scrollback above it, with ansi as escape sequences.")
        .level(level(Grant::Use));
    methods
        .add("terminals.sessions", |call: Call, p: rookery_proto::terminals::EnvSessionQuery| async move {
            let name = crate::name_of(&p.env).to_owned();
            let env = crate::uri(&name);
            let manages = rookery_facilities::access::holds(&call, &env, Grant::Manage).await?;
            let remote = link(&call, &name).await?;
            let listed = ask(&remote, "GET", "/v1/sessions", None).await?;
            let listed: Vec<rookery_api::Session> =
                serde_json::from_value(listed).map_err(|e| RpcError::internal(e.to_string()))?;
            let mut sessions = vec![];
            for s in listed {
                let held = call.store().terminals().by_session(&name, &s.id).await?;
                let visible = match &held {
                    Some(t) => level_of(&call, t).await?.is_some(),
                    None => false,
                };
                let terminal = held.as_ref().filter(|_| visible).map(|t| t.id.clone());
                let running = s.ended.is_none();
                let preview = match p.preview && running && s.kind == rookery_api::SessionKind::Terminal && (manages || visible) {
                    true => ask(&remote, "GET", &session_path(&s.id, "/screen?tail=5"), None)
                        .await
                        .ok()
                        .and_then(|v| v["text"].as_str().map(str::to_owned)),
                    false => None,
                };
                sessions.push(EnvSession {
                    kind: match s.kind {
                        rookery_api::SessionKind::Terminal => "terminal".into(),
                        _ => "process".into(),
                    },
                    id: s.id,
                    title: s.title,
                    command: s.command,
                    user: s.user,
                    started: s.started,
                    ended: s.ended,
                    exit_code: s.exit_code,
                    lost: s.lost,
                    terminal,
                    adopted: held.is_some(),
                    preview,
                });
            }
            Ok(EnvSessionList { sessions })
        })
        .summary("The sessions on a host, each with the terminal it is when the caller may see that one. Previews of a screen are for the host's managers and a terminal's owner.")
        .level(crate::env::level(Grant::View));
    methods
        .add("terminals.adopt", |call: Call, p: TerminalAdopt| async move {
            let name = crate::name_of(&p.env).to_owned();
            let remote = link(&call, &name).await?;
            let s = ask(&remote, "GET", &session_path(&p.session, ""), None).await?;
            let s: rookery_api::Session =
                serde_json::from_value(s).map_err(|e| RpcError::internal(e.to_string()))?;
            if s.kind != rookery_api::SessionKind::Terminal || s.ended.is_some() {
                return Err(RpcError::invalid(format!("{} is no running terminal", p.session)));
            }
            let run_as = match s.user.as_str() {
                "operator" | "root" => s.user.clone(),
                _ => "agent".into(),
            };
            let id = new_id();
            let terminal = call
                .store()
                .terminals()
                .insert(New {
                    id: &id,
                    host: &name,
                    session: &s.id,
                    owner: call.user_id().ok(),
                    run_as: &run_as,
                    title: &s.title,
                    command: &json!(s.command),
                    cwd: None,
                    cols: s.cols.unwrap_or(80) as i32,
                    rows: s.rows.unwrap_or(24) as i32,
                })
                .await?;
            call.audit("terminal.adopt", Some(&crate::uri(&name)), json!({"terminal": id, "session": s.id})).await;
            changed(&call.kernel, &terminal).await;
            let level = level_of(&call, &terminal).await?;
            Ok(info(&call.kernel, &terminal, level))
        })
        .summary("Makes a terminal session of a host that no terminal holds the caller's terminal. A session the server did not open for them is someone else's, so this needs manage.")
        .level(crate::env::level(Grant::Manage));
}

/// Keeps a terminal session that `hosts.sessions.open` opened as the caller's terminal.
pub async fn keep_opened(
    call: &Call,
    host: &str,
    start: &rookery_api::StartSession,
    started: &Value,
) {
    let Some(session) = started["id"].as_str() else {
        return;
    };
    let command = match &start.command {
        rookery_api::CommandLine::Args(args) => json!(args),
        rookery_api::CommandLine::Shell(script) => json!([script]),
    };
    let id = new_id();
    let run_as = match start.user.as_str() {
        "operator" | "root" => start.user.as_str(),
        _ => "agent",
    };
    let kept = call
        .store()
        .terminals()
        .insert(New {
            id: &id,
            host,
            session,
            owner: call.user_id().ok(),
            run_as,
            title: started["title"].as_str().unwrap_or_default(),
            command: &command,
            cwd: start.cwd.as_deref(),
            cols: start.cols.unwrap_or(80) as i32,
            rows: start.rows.unwrap_or(24) as i32,
        })
        .await;
    match kept {
        Ok(terminal) => changed(&call.kernel, &terminal).await,
        Err(e) => tracing::warn!(error = %e, %host, "keeping a terminal's record failed"),
    }
}

fn new_id() -> String {
    format!("term_{}", uuid::Uuid::now_v7().simple())
}

/// Notes who typed into a terminal, at most once a second.
async fn typed(call: &Call, id: &str, via: &str, bytes: u64) {
    let principal = call.principal().uri();
    let input = InputInfo {
        principal,
        name: call.user().map(|u| u.name.clone()),
        at: chrono::Utc::now().timestamp_millis(),
        via: via.into(),
        bytes,
    };
    if let Ok(value) = serde_json::to_value(&input) {
        let _ = call.store().terminals().set_last_input(id, &value).await;
    }
}

/// Gives a terminal a size on its host and tells its viewers, with the viewer it follows.
async fn resize(
    kernel: &Kernel,
    call: &Call,
    t: &Terminal,
    size: TermSize,
    active: Option<String>,
) -> Result<(), RpcError> {
    let size = TermSize {
        cols: size.cols.clamp(2, 1000),
        rows: size.rows.clamp(2, 1000),
    };
    let remote = link(call, &t.host).await?;
    ask(
        &remote,
        "POST",
        &session_path(&t.session, "/resize"),
        Some(&json!({"rows": size.rows, "cols": size.cols})),
    )
    .await?;
    call.store()
        .terminals()
        .set_size(&t.id, size.cols as i32, size.rows as i32)
        .await?;
    let hub = Hub::of(kernel);
    if let Some(live) = hub.live.lock().unwrap().get(&t.id) {
        live.state.lock().unwrap().size = size.clone();
    }
    hub.tell(
        &t.id,
        TerminalEvent::Size {
            cols: size.cols,
            rows: size.rows,
            active,
        },
    );
    Ok(())
}

// ── Attaching ───────────────────────────────────────────────────────────────

/// What rookeryd says first on a session's stream asked for with `attached=1`.
#[derive(serde::Deserialize)]
struct Header {
    snapshot: bool,
    from: u64,
    next: u64,
    frames: usize,
}

fn header_of(frame: &Frame) -> Option<Header> {
    let Frame::Text(text) = frame else {
        return None;
    };
    let value: Value = serde_json::from_str(text.as_str()).ok()?;
    serde_json::from_value(value.get("attached")?.clone()).ok()
}

async fn upstream(
    remote: &crate::serve::Remote,
    session: &str,
    after: Option<u64>,
) -> Result<rookery_core::link::LinkStream, RpcError> {
    let link = remote
        .link
        .as_ref()
        .ok_or_else(|| RpcError::unavailable("no link"))?;
    let since = after.map(|a| format!("&since={a}")).unwrap_or_default();
    link.stream(&session_path(session, &format!("?attached=1{since}")))
        .await
        .map_err(|e| api::Fail::from(e).into())
}

async fn first(stream: &mut rookery_core::link::LinkStream) -> Result<Frame, RpcError> {
    match tokio::time::timeout(std::time::Duration::from_secs(15), stream.next()).await {
        Ok(Some(Ok(frame))) => Ok(frame),
        Ok(_) => Err(RpcError::unavailable(
            "the host closed the terminal's stream",
        )),
        Err(_) => Err(RpcError::timeout(
            "the host did not answer for the terminal",
        )),
    }
}

fn framed(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 1);
    out.push(kind);
    out.extend_from_slice(body);
    out
}

fn event_frame(event: &TerminalEvent) -> Vec<u8> {
    framed(FRAME_EVENT, &serde_json::to_vec(event).unwrap_or_default())
}

/// Most of a frame's payload, with room for the type byte.
const CARRIED: usize = rookery_proto::stream::MAX_FRAME_PAYLOAD - 1;

async fn attach(call: Call, p: TerminalAttach) -> Result<TerminalAttached, RpcError> {
    let t = record(&call, &p.id).await?;
    let remote = link(&call, &t.host).await?;
    // Resume when what is missed is still on the host and not much; else start with the screen.
    let mut after = p.after;
    let mut host = upstream(&remote, &t.session, after).await?;
    let mut said = first(&mut host).await?;
    if let (Some(wanted), Some(header)) = (after, header_of(&said))
        && (header.snapshot
            || header.from > wanted
            || wanted > header.next
            || header.next - wanted > RESUME_AT_MOST)
    {
        drop(host);
        after = None;
        host = upstream(&remote, &t.session, None).await?;
        said = first(&mut host).await?;
    }
    let header = header_of(&said);
    let (mode, offset, mut replay) = match (&header, after) {
        (Some(h), _) if h.snapshot => (AttachMode::Snapshot, h.next, Vec::new()),
        (Some(h), _) => (AttachMode::Resume, h.from, Vec::new()),
        // A controller from before the header: its first frame is already the replay.
        (None, Some(a)) => (AttachMode::Resume, a, vec![said.clone()]),
        (None, None) => (AttachMode::Snapshot, t.offset as u64, vec![said.clone()]),
    };
    let mut snapshot = Vec::new();
    if mode == AttachMode::Snapshot {
        let frames = header.as_ref().map_or(0, |h| h.frames);
        for _ in 0..frames {
            replay.push(first(&mut host).await?);
        }
        for frame in replay.drain(..) {
            if let Frame::Binary(bytes) = frame {
                snapshot.extend_from_slice(&bytes);
            }
        }
    }
    let (live, viewer) = Hub::of(&call.kernel).join(&t, p.size.clone(), p.readonly);
    let size = live.state.lock().unwrap().size.clone();
    let stream = match call
        .conn()
        .and_then(|conn| conn.streams().open(Initiator::Peer))
    {
        Ok(stream) => stream,
        Err(e) => {
            Hub::of(&call.kernel).leave(&t.id, &live, &viewer);
            return Err(e);
        }
    };
    let level = level_of(&call, &t).await?;
    let attached = TerminalAttached {
        stream: stream.id(),
        mode,
        offset,
        snapshot: snapshot.len() as u64,
        size,
        viewer: viewer.clone(),
        readonly: p.readonly,
        info: info(&call.kernel, &t, level),
    };
    let pump = Pump {
        call: call.clone(),
        terminal: t,
        live,
        viewer,
        readonly: p.readonly,
        offset,
        size: p.size,
    };
    tokio::spawn(pump.run(host, stream, snapshot, replay));
    Ok(attached)
}

/// One viewer's stream, both ways, until either side ends it.
struct Pump {
    call: Call,
    terminal: Terminal,
    live: Arc<Live>,
    viewer: String,
    readonly: bool,
    offset: u64,
    size: Option<TermSize>,
}

impl Pump {
    async fn run(
        mut self,
        host: rookery_core::link::LinkStream,
        stream: rookery_kernel::streams::Stream,
        snapshot: Vec<u8>,
        replay: Vec<Frame>,
    ) {
        let kernel = self.call.kernel.clone();
        let hub = Hub::of(&kernel);
        let mut events = self.live.events.subscribe();
        let (mut reader, writer) = stream.split();
        let (mut to_host, mut from_host) = host.split();
        let mut open = true;
        for chunk in snapshot.chunks(CARRIED) {
            open = open && writer.write(&framed(FRAME_SNAPSHOT, chunk)).await.is_ok();
        }
        for frame in replay {
            open = open && self.output(&writer, frame).await;
        }
        let count = self.live.state.lock().unwrap().viewers.len() as u32;
        let _ = self.live.events.send(TerminalEvent::Viewers { count });
        open = open
            && writer
                .write(&event_frame(&TerminalEvent::Ready))
                .await
                .is_ok();
        // A first viewer that can type gives its size.
        if !self.readonly
            && let Some(size) = self.size.clone()
        {
            let first = {
                let mut state = self.live.state.lock().unwrap();
                let first = state.active.is_none();
                if first {
                    state.active = Some(self.viewer.clone());
                }
                first
            };
            if first {
                let _ = resize(
                    &kernel,
                    &self.call,
                    &self.terminal,
                    size,
                    Some(self.viewer.clone()),
                )
                .await;
            }
        }
        changed(&kernel, &self.terminal).await;
        let mut typed_at = std::time::Instant::now() - std::time::Duration::from_secs(5);
        let mut typed_bytes = 0u64;
        while open {
            tokio::select! {
                frame = from_host.next() => match frame {
                    Some(Ok(frame)) => open = self.output(&writer, frame).await,
                    _ => open = false,
                },
                inbound = reader.read() => match inbound {
                    Some(Inbound::Data(data)) => {
                        let Some((&kind, body)) = data.split_first() else { continue };
                        match kind {
                            FRAME_OUTPUT if !self.readonly => {
                                if to_host.send(Frame::Binary(body.to_vec().into())).await.is_err() {
                                    open = false;
                                }
                                typed_bytes += body.len() as u64;
                                if typed_at.elapsed() > std::time::Duration::from_secs(1) {
                                    typed(&self.call, &self.terminal.id, "viewer", typed_bytes).await;
                                    typed_at = std::time::Instant::now();
                                    typed_bytes = 0;
                                }
                            }
                            FRAME_EVENT if !self.readonly => self.control(&kernel, body).await,
                            _ => {}
                        }
                    }
                    Some(Inbound::Eof) => {}
                    Some(Inbound::Closed(_)) | None => open = false,
                },
                event = events.recv() => match event {
                    Ok(event) => {
                        let ends = matches!(event, TerminalEvent::State { state, .. } if state != TerminalState::Running);
                        open = writer.write(&event_frame(&event)).await.is_ok() && !ends;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => open = false,
                },
            }
        }
        writer.close(None);
        let (count, size) = hub.leave(&self.terminal.id, &self.live, &self.viewer);
        let _ = self.live.events.send(TerminalEvent::Viewers { count });
        if let Some(size) = size {
            let active = self.live.state.lock().unwrap().active.clone();
            let _ = resize(&kernel, &self.call, &self.terminal, size, active).await;
        }
        let _ = kernel
            .store
            .terminals()
            .set_offset(&self.terminal.id, self.offset as i64)
            .await;
        changed(&kernel, &self.terminal).await;
    }

    /// A frame of the host's towards the viewer. False once the stream is to end.
    async fn output(
        &mut self,
        writer: &rookery_kernel::streams::OwnedWriter,
        frame: Frame,
    ) -> bool {
        match frame {
            Frame::Binary(bytes) => {
                for chunk in bytes.chunks(CARRIED) {
                    if writer.write(&framed(FRAME_OUTPUT, chunk)).await.is_err() {
                        return false;
                    }
                }
                self.offset += bytes.len() as u64;
                let mut state = self.live.state.lock().unwrap();
                state.offset = state.offset.max(self.offset);
                true
            }
            Frame::Text(text) => {
                let value: Value = serde_json::from_str(text.as_str()).unwrap_or_default();
                if let Some(exited) = value.get("exited") {
                    let code = exited["exit_code"].as_i64().map(|c| c as i32);
                    let store = self.call.kernel.store.terminals();
                    if store
                        .end(&self.terminal.id, "exited", code)
                        .await
                        .unwrap_or(false)
                    {
                        self.terminal.state = "exited".into();
                        self.terminal.exit_code = code;
                        let _ = store
                            .set_offset(&self.terminal.id, self.offset as i64)
                            .await;
                        changed(&self.call.kernel, &self.terminal).await;
                    }
                    let _ = self.live.events.send(TerminalEvent::State {
                        state: TerminalState::Exited,
                        exit_code: code,
                    });
                    let _ = writer
                        .write(&event_frame(&TerminalEvent::State {
                            state: TerminalState::Exited,
                            exit_code: code,
                        }))
                        .await;
                    return false;
                }
                // Output was dropped on the way: the viewer attaches again from its offset.
                value.get("dropped").is_none()
            }
            _ => true,
        }
    }

    async fn control(&mut self, kernel: &Kernel, body: &[u8]) {
        let Ok(control) = serde_json::from_slice::<AttachControl>(body) else {
            return;
        };
        let apply = {
            let mut state = self.live.state.lock().unwrap();
            match control {
                AttachControl::Resize { cols, rows } => {
                    let size = TermSize { cols, rows };
                    if let Some(v) = state.viewers.get_mut(&self.viewer) {
                        v.size = Some(size.clone());
                    }
                    self.size = Some(size.clone());
                    let ours = state.active.is_none()
                        || state.active.as_deref() == Some(self.viewer.as_str());
                    if ours {
                        state.active = Some(self.viewer.clone());
                    }
                    ours.then_some(size)
                }
                AttachControl::Active => {
                    state.active = Some(self.viewer.clone());
                    self.size.clone()
                }
            }
        };
        if let Some(size) = apply {
            let _ = resize(
                kernel,
                &self.call,
                &self.terminal,
                size,
                Some(self.viewer.clone()),
            )
            .await;
        }
    }
}
