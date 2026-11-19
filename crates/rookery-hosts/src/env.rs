//! Hosts as environments: `env.*`, a projection of the saved hosts for clients that work on them.
//!
//! `view` on `env://NAME` sees a host, its state and who works there. `use` reaches its files inside
//! its roots and its loopback ports, as the agent account. Nothing here writes as another account.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use rookery_core::host_type::Capability;
use rookery_kernel::streams::Initiator;
use rookery_kernel::{Call, Empty, Grant, Level, Methods, RpcError};
use rookery_proto::env::*;
use rookery_proto::hosts::HostInfo;
use rookery_proto::stream::StreamOpened;
use rookery_proto::terminals::EnvUser;
use serde_json::json;
use sha2::Digest;

use crate::serve::{Serve, api};

/// The largest file `env.readFile` and `env.writeFile` carry, as a call's message is at most 12 MiB.
pub const MAX_FILE: usize = 8 * 1024 * 1024;

/// The most `env.exec` keeps of each output stream. It goes on reading past this, so that a
/// command is not held up by a pipe nobody empties.
const MAX_COLLECT: usize = 2 * 1024 * 1024;

/// The most of an `env.exec` result once it is encoded for the wire. A text frame on `/ws` is at
/// most 2 MiB, and `truncated` says when a command wrote more than this.
const MAX_RESULT: usize = 1536 * 1024;

/// How long a command has without `timeoutMs`, and the longest it may ask for.
const EXEC_TIMEOUT: Duration = Duration::from_secs(60);
const EXEC_LONGEST: Duration = Duration::from_secs(60 * 60);

/// How long cloning and worktrees have: git may fetch a great deal.
const GIT_TIMEOUT: Duration = Duration::from_secs(600);

/// A level on the host the param `env` names.
pub fn level(grant: Grant) -> Level {
    Level::Resource {
        param: "env".into(),
        grant,
        scheme: Some("env".into()),
    }
}

pub(crate) fn serve(call: &Call) -> Result<Arc<Serve>, RpcError> {
    crate::hosts(&call.kernel).serve()
}

/// What a host of this type can do.
pub fn supports(host_type: rookery_api::HostType) -> EnvSupports {
    let can = |capability| rookery_core::host_type::check("", host_type, capability).is_ok();
    let sessions = can(Capability::Sessions);
    EnvSupports {
        terminal: sessions,
        process: sessions,
        files: sessions,
        connect: sessions,
        agents: None,
        checkouts: sessions,
        desktop: can(Capability::Desktop),
        privileged: sessions,
        persistent_sessions: sessions,
        deploy: false,
    }
}

fn kind_of(host_type: rookery_api::HostType) -> String {
    serde_json::to_value(host_type)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// A host as an environment, for a caller at `level`.
pub fn info_of(host: &HostInfo, owner: Option<String>, level: Option<Grant>) -> EnvInfo {
    let state = match host.reachable {
        true => EnvState::Online,
        false => EnvState::Offline,
    };
    let mut supported = supports(host.host_type);
    supported.agents = Some(host.agents);
    EnvInfo {
        name: host.name.clone(),
        uri: crate::uri(&host.name),
        kind: kind_of(host.host_type),
        state,
        owner,
        level,
        os: None,
        version: host.version.clone(),
        supports: supported,
        error: (!host.reachable).then(|| {
            host.error
                .clone()
                .unwrap_or_else(|| "it does not answer".into())
        }),
    }
}

/// The host as `env.get` gives it to this caller, or `None` when they may not see it.
pub async fn visible(call: &Call, host: &HostInfo) -> Result<Option<EnvInfo>, RpcError> {
    let uri = crate::uri(&host.name);
    let level = rookery_facilities::access::level(call, &uri).await?;
    if level.is_none() {
        return Ok(None);
    }
    let owner = call.store().access().owner(&uri).await?;
    Ok(Some(info_of(host, owner.map(|u| u.to_string()), level)))
}

/// The link of a host that has files and ports, or why not.
async fn linked(call: &Call, env: &str) -> Result<Arc<crate::serve::Remote>, RpcError> {
    let state = serve(call)?;
    Ok(api::controller(&state, crate::name_of(env), Capability::Sessions).await?)
}

fn link_of(remote: &crate::serve::Remote) -> Result<&rookery_core::link::Link, RpcError> {
    remote
        .link
        .as_ref()
        .ok_or_else(|| RpcError::unavailable("no link"))
}

/// What the host's controller said when it refused a request.
fn refused(path: &str, status: u16, body: &[u8]) -> RpcError {
    let said: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let message = said["error"]["message"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| String::from_utf8_lossy(body).trim().to_owned());
    match status {
        404 => RpcError::not_found(format!("nothing at {path}")),
        403 => RpcError::forbidden(message),
        409 | 412 => RpcError::conflict(message),
        _ => RpcError::invalid(message),
    }
}

fn files_target(path: &str) -> String {
    format!("/v1/files?path={}", rookery_api::client::encode(path))
}

/// A file's bytes, or `None` when nothing is there.
pub(crate) async fn read_bytes(
    link: &rookery_core::link::Link,
    path: &str,
) -> Result<Option<Vec<u8>>, RpcError> {
    let range = format!("bytes=0-{MAX_FILE}");
    let answer = link
        .send("GET", &files_target(path), &[("range", &range)], &[])
        .await
        .map_err(api::Fail::from)?;
    if answer.status == 404 {
        return Ok(None);
    }
    if answer.status >= 400 {
        return Err(refused(path, answer.status, &answer.body));
    }
    let is_listing = answer
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if is_listing {
        return Err(RpcError::invalid(format!("{path} is a folder")));
    }
    if answer.body.len() > MAX_FILE {
        return Err(RpcError::invalid(format!(
            "{path} is larger than the {} MiB a call carries",
            MAX_FILE >> 20
        )));
    }
    Ok(Some(answer.body.to_vec()))
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

async fn listing(
    link: &rookery_core::link::Link,
    path: &str,
) -> Result<Option<rookery_api::Listing>, RpcError> {
    let answer = link
        .send("GET", &files_target(path), &[("range", "bytes=0-0")], &[])
        .await
        .map_err(api::Fail::from)?;
    if answer.status == 404 {
        return Ok(None);
    }
    if answer.status >= 400 {
        return Err(refused(path, answer.status, &answer.body));
    }
    let is_listing = answer
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if !is_listing {
        return Err(RpcError::invalid(format!("{path} is not a folder")));
    }
    serde_json::from_slice(&answer.body)
        .map(Some)
        .map_err(|e| RpcError::internal(format!("the host's listing: {e}")))
}

fn entry_of(entry: rookery_api::FileEntry) -> DirEntry {
    DirEntry {
        kind: match entry.kind.as_str() {
            "file" => FileKind::File,
            "dir" => FileKind::Dir,
            "link" => FileKind::Link,
            _ => FileKind::Other,
        },
        name: entry.name,
        size: entry.size,
        modified: (!entry.modified.is_empty()).then_some(entry.modified),
    }
}

/// Writes to one path of a host one at a time, so that a compare-and-swap of this server is not
/// overtaken by another write of its own.
#[derive(Default)]
struct WriteLocks(Mutex<HashMap<(String, String), PathLock>>);

type PathLock = Arc<tokio::sync::Mutex<()>>;

impl WriteLocks {
    fn of(&self, host: &str, path: &str) -> PathLock {
        let mut all = self.0.lock().unwrap();
        all.retain(|_, lock| Arc::strong_count(lock) > 1);
        all.entry((host.to_owned(), path.to_owned()))
            .or_default()
            .clone()
    }
}

fn lexical_clean(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        "/".into()
    } else {
        format!("/{}", parts.join("/"))
    }
}

/// Resolves the canonical path via `rookeryd`'s helper and acquires the per-host write lock on it.
pub(crate) async fn lock_path(
    call: &Call,
    remote: &crate::serve::Remote,
    path: &str,
) -> Result<tokio::sync::OwnedMutexGuard<()>, RpcError> {
    let canonical = resolved(remote, path).await?;
    let lock = call
        .kernel
        .facility::<WriteLocks>()
        .of(&remote.name, &canonical);
    Ok(lock.lock_owned().await)
}

/// A path as the host's helper resolves it: symlinks and `.` are gone, and a path that is not
/// there yet is cleaned of `..`, so anything that would lead out of a root is refused.
async fn resolved(remote: &crate::serve::Remote, path: &str) -> Result<String, RpcError> {
    let answer = link_of(remote)?
        .send(
            "GET",
            &format!("{}&resolve=true", files_target(path)),
            &[],
            &[],
        )
        .await
        .map_err(api::Fail::from)?;
    if answer.status == 200 {
        let parsed = serde_json::from_slice::<serde_json::Value>(&answer.body).ok();
        match parsed
            .as_ref()
            .filter(|v| v.get("entries").is_none())
            .and_then(|v| v["path"].as_str())
        {
            Some(canon) => Ok(canon.to_owned()),
            None => Ok(lexical_clean(path)),
        }
    } else if answer.status == 404 {
        Ok(lexical_clean(path))
    } else {
        Err(refused(path, answer.status, &answer.body))
    }
}

/// Whether `path` is `folder` or inside it.
fn within(path: &str, folder: &str) -> bool {
    let folder = folder.trim_end_matches('/');
    path == folder
        || path
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The folders a host keeps: its roots, and where its checkouts go.
async fn host_paths(remote: &crate::serve::Remote) -> Result<rookery_api::Paths, RpcError> {
    let answer = api::pass(remote, "GET", "/v1/paths", None).await?;
    serde_json::from_value(answer).map_err(|e| RpcError::internal(format!("the host's paths: {e}")))
}

/// Where checkouts go on a host, or why there are none.
async fn checkouts_folder(remote: &crate::serve::Remote) -> Result<String, RpcError> {
    host_paths(remote).await?.workspaces.ok_or_else(|| {
        RpcError::unavailable("this host keeps no checkouts: it has no folder for them")
    })
}

/// Ends a session on a host when this call ends, by its own end or not: a command whose caller
/// went away does not run on.
struct SessionEnd {
    remote: Arc<crate::serve::Remote>,
    id: String,
}

impl Drop for SessionEnd {
    fn drop(&mut self) {
        let remote = self.remote.clone();
        let target = format!("/v1/sessions/{}", rookery_api::client::encode(&self.id));
        tokio::spawn(async move {
            let _ = api::pass(&remote, "DELETE", &target, None).await;
        });
    }
}

/// What a command wrote and how it ended.
struct Output {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: Option<i32>,
    lost: bool,
    truncated: bool,
}

/// Keeps as much of a chunk as there is room for, and says whether any of it was left out.
fn keep(target: &mut Vec<u8>, bytes: &[u8]) -> bool {
    let room = MAX_COLLECT.saturating_sub(target.len());
    let kept = bytes.len().min(room);
    target.extend_from_slice(&bytes[..kept]);
    kept == bytes.len()
}

/// Runs a process session on the host and follows its output, frame by frame and with the host's
/// own flow, to its end. The session is ended if anything else ends the call: its deadline, a
/// revoked level, or the caller's connection going.
async fn run_process(
    call: &Call,
    remote: &Arc<crate::serve::Remote>,
    start: rookery_api::StartSession,
    limit: Duration,
    need: Grant,
) -> Result<Output, RpcError> {
    let body = serde_json::to_value(&start).map_err(|e| RpcError::internal(e.to_string()))?;
    let started = api::pass(remote, "POST", "/v1/sessions", Some(&body)).await?;
    let id = started["id"]
        .as_str()
        .ok_or_else(|| RpcError::internal("the host named no session"))?
        .to_owned();
    let ended = SessionEnd {
        remote: remote.clone(),
        id: id.clone(),
    };
    // `attached` says where the replay begins, so output the host no longer keeps is noticed.
    let frames = link_of(remote)?
        .stream(&format!(
            "/v1/sessions/{}?since=0&attached=1",
            rookery_api::client::encode(&id)
        ))
        .await
        .map_err(api::Fail::from)?;
    let (_input, mut output) = frames.split();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code = None;
    let mut finished = false;
    let mut truncated = false;
    let mut access = crate::access_watch(call, crate::uri(&remote.name), need).subscribe();
    let collect = async {
        while let Some(frame) = output.next().await {
            match frame {
                Ok(rookery_api::client::Frame::Binary(data)) => {
                    let Some((&stream, bytes)) = data.split_first() else {
                        continue;
                    };
                    let kept = match stream {
                        1 => keep(&mut stdout, bytes),
                        2 => keep(&mut stderr, bytes),
                        _ => continue,
                    };
                    truncated |= !kept;
                }
                Ok(rookery_api::client::Frame::Text(text)) => {
                    let said = serde_json::from_str::<serde_json::Value>(text.as_str()).ok();
                    let said = said.as_ref();
                    if let Some(attached) = said.and_then(|v| v.get("attached")) {
                        // The replay begins after the start when the host dropped what came first.
                        if attached["from"].as_u64().unwrap_or(0) > 0 {
                            truncated = true;
                        }
                    }
                    if said.is_some_and(|v| v.get("dropped").is_some()) {
                        truncated = true;
                    }
                    if let Some(exited) = said.and_then(|v| v.get("exited")) {
                        exit_code = exited["exit_code"].as_i64().map(|c| c as i32);
                        finished = true;
                        return Ok(());
                    }
                }
                Ok(rookery_api::client::Frame::Close(_)) => return Ok(()),
                // Ping, pong and a frame of a kind the session never sends.
                Ok(_) => {}
                Err(e) => {
                    return Err(RpcError::unavailable(format!(
                        "the host's output ended: {e}"
                    )));
                }
            }
        }
        Ok(())
    };
    tokio::select! {
        result = collect => result?,
        () = access.wait_revoked() => {
            return Err(RpcError::forbidden("access revoked"));
        }
        () = tokio::time::sleep(limit) => {
            return Err(RpcError::timeout(format!(
                "the command did not finish within {}",
                seconds(limit)
            )));
        }
    }
    // No exit frame: the host's controller went, and the command with it.
    drop(ended);
    Ok(Output {
        stdout,
        stderr,
        exit_code,
        lost: !finished,
        truncated,
    })
}

/// The longest prefix of `text` whose JSON encoding fits `budget` bytes, and whether it was cut.
fn prefix_within(text: &str, budget: usize) -> (&str, bool) {
    let encoded = |text: &str| serde_json::to_string(text).map_or(text.len(), |json| json.len());
    if text.is_empty() || encoded(text) <= budget {
        return (text, false);
    }
    let (mut low, mut high) = (0, text.len());
    while low < high {
        let mut middle = low + (high - low).div_ceil(2);
        while !text.is_char_boundary(middle) {
            middle -= 1;
        }
        if middle <= low {
            break;
        }
        if encoded(&text[..middle]) <= budget {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    (&text[..low], true)
}

/// An `ExecResult`'s two streams within the bytes one message may hold: half for each at first,
/// then whatever one leaves to the other.
fn fit(stdout: &[u8], stderr: &[u8]) -> (String, String, bool) {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    let half = MAX_RESULT / 2;
    let (stderr, cut_err) = prefix_within(&stderr, half);
    let (stdout, cut_out) = prefix_within(&stdout, MAX_RESULT.saturating_sub(encoded(stderr)));
    let (stderr, cut_more) = prefix_within(stderr, MAX_RESULT.saturating_sub(encoded(stdout)));
    (
        stdout.to_owned(),
        stderr.to_owned(),
        cut_err || cut_out || cut_more,
    )
}

fn encoded(text: &str) -> usize {
    serde_json::to_string(text).map_or(text.len(), |json| json.len())
}

/// An address without the credential in it, for a log, a session or a listing.
fn strip_userinfo(address: &str) -> String {
    let Some((scheme, rest)) = address.split_once("://") else {
        return address.to_owned();
    };
    let (authority, tail) = match rest.split_once('/') {
        Some((authority, tail)) => (authority, Some(tail)),
        None => (rest, None),
    };
    let Some((_, host)) = authority.rsplit_once('@') else {
        return address.to_owned();
    };
    if host.is_empty() {
        return address.to_owned();
    }
    match tail {
        Some(tail) => format!("{scheme}://{host}/{tail}"),
        None => format!("{scheme}://{host}"),
    }
}

/// An HTTP address split into the address without its credential and the credential itself, so
/// that git's argv, which a host's session list shows, holds no token.
fn split_credentials(address: &str) -> (String, Option<String>) {
    let Some((scheme, rest)) = address.split_once("://") else {
        return (address.to_owned(), None);
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return (address.to_owned(), None);
    }
    let (authority, tail) = match rest.split_once('/') {
        Some((authority, tail)) => (authority, Some(tail)),
        None => (rest, None),
    };
    let Some((userinfo, host)) = authority.rsplit_once('@') else {
        return (address.to_owned(), None);
    };
    if host.is_empty() || userinfo.is_empty() {
        return (address.to_owned(), None);
    }
    // `user:password`, or a token where the user name goes and no password.
    let credential = match userinfo.contains(':') {
        true => userinfo.to_owned(),
        false => format!("{userinfo}:"),
    };
    let cleaned = match tail {
        Some(tail) => format!("{scheme}://{host}/{tail}"),
        None => format!("{scheme}://{host}"),
    };
    (cleaned, Some(credential))
}

/// Refuses an address whose transport is git's own way to run a command, such as `ext::sh …`.
fn check_address(address: &str) -> Result<(), RpcError> {
    let transport = address.split_once("::").filter(|(scheme, _)| {
        !scheme.is_empty()
            && !scheme.contains('/')
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    });
    match transport {
        Some((scheme, _)) => Err(RpcError::invalid(format!(
            "{} is not an address Rookery clones: it names the transport {scheme}, not HTTPS, SSH or a folder",
            strip_userinfo(address)
        ))),
        None => Ok(()),
    }
}

fn seconds(limit: Duration) -> String {
    match limit.as_secs() {
        0 => format!("{} ms", limit.as_millis()),
        seconds => format!("{seconds} s"),
    }
}

/// Runs git to its end as the agent account, with settings of its own: no transport that runs a
/// command of the address's choosing, no prompt that waits for someone who is not there, and no
/// locale in its messages.
async fn git_with(
    call: &Call,
    remote: &Arc<crate::serve::Remote>,
    configs: &[&str],
    args: &[&str],
    cwd: Option<&str>,
    limit: Duration,
    vars: BTreeMap<String, String>,
) -> Result<Output, RpcError> {
    let mut command = vec!["git".to_string()];
    command.extend(configs.iter().map(|c| (*c).to_string()));
    command.extend(args.iter().map(|a| (*a).to_string()));
    let mut env: BTreeMap<String, String> = [
        // A folder on the host is what the agent already reads; `ext::`, which runs a command,
        // is what must not pass.
        (
            "GIT_ALLOW_PROTOCOL".to_string(),
            "https:ssh:file".to_string(),
        ),
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        ("LC_ALL".to_string(), "C".to_string()),
    ]
    .into();
    env.extend(vars);
    let out = run_process(
        call,
        remote,
        rookery_api::StartSession {
            kind: rookery_api::SessionKind::Process,
            command: rookery_api::CommandLine::Args(command),
            user: "agent".into(),
            cwd: cwd.map(str::to_owned),
            env,
            input: None,
            input_b64: None,
            title: Some(format!(
                "{}: git {}",
                remote.name,
                args.first().unwrap_or(&"")
            )),
            // The host ends it too, should this server stop without ending the session.
            timeout: Some(limit.as_secs()),
            rows: None,
            cols: None,
        },
        limit,
        Grant::Use,
    )
    .await?;
    if out.exit_code != Some(0) {
        let said = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        let why = match out.exit_code {
            Some(code) => format!("git {} failed ({code}): {said}", args.join(" ")),
            None => format!("git {} was lost on {}", args.join(" "), remote.name),
        };
        return Err(
            RpcError::invalid(why).with_details(json!({"stderr": said, "exitCode": out.exit_code}))
        );
    }
    Ok(out)
}

/// [`git_with`] for the calls that need no setting of their own.
async fn git(
    call: &Call,
    remote: &Arc<crate::serve::Remote>,
    args: &[&str],
    cwd: Option<&str>,
    limit: Duration,
) -> Result<Output, RpcError> {
    git_with(call, remote, &[], args, cwd, limit, BTreeMap::new()).await
}

/// A folder and file name for a checkout: letters, digits, `.`, `-` and `_`.
fn check_name(name: &str) -> Result<(), RpcError> {
    let ok = !name.is_empty()
        && name != "."
        && name != ".."
        && !name.starts_with('-')
        && name.len() <= 200
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    if ok {
        Ok(())
    } else {
        Err(RpcError::invalid(format!(
            "{name:?} is not a checkout name: use letters, digits, '.', '-' and '_'"
        )))
    }
}

/// A branch a worktree can be made on. git checks refs itself; this refuses what would reach it
/// as an option or a path.
fn check_branch(branch: &str) -> Result<(), RpcError> {
    let bad = branch.is_empty()
        || branch.starts_with('-')
        || branch.ends_with('/')
        || branch.ends_with(".lock")
        || branch.contains("..")
        || branch.contains("@{")
        || branch
            .chars()
            .any(|c| c.is_whitespace() || "~^:?*[\\\x7f".contains(c));
    if bad {
        Err(RpcError::invalid(format!(
            "{branch:?} is not a branch name"
        )))
    } else {
        Ok(())
    }
}

/// What a worktree may start from: a commit, a tag or a branch, as git reads them, and not
/// something git would take for an option or a second argument.
fn check_rev(rev: &str) -> Result<(), RpcError> {
    let bad = rev.is_empty()
        || rev.starts_with('-')
        || rev.chars().any(|c| c.is_whitespace() || c.is_control());
    if bad {
        Err(RpcError::invalid(format!("{rev:?} is not a revision")))
    } else {
        Ok(())
    }
}

/// The name an address suggests: its last segment without `.git`, and nothing but what a folder
/// name may hold.
fn name_from_url(url: &str) -> String {
    let last = url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("repo");
    let name: String = last
        .trim_end_matches(".git")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    if name.is_empty() { "repo".into() } else { name }
}

/// The checkouts in a host's folder: one process for the folder, with the discoveries of the
/// prototype's `env.checkouts`: each folder that holds a `.git`, its branch, HEAD, remote and
/// whether `.git` is a file, which is what a worktree has.
async fn checkouts_of(
    call: &Call,
    remote: &Arc<crate::serve::Remote>,
    folder: &str,
    need: Grant,
) -> Result<Vec<Checkout>, RpcError> {
    const SCRIPT: &str = r#"cd "$1" 2>/dev/null || exit 0
for d in */; do
  d=${d%/}
  [ -e "$d/.git" ] || continue
  wt=0; [ -f "$d/.git" ] && wt=1
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$d" "$(git -C "$d" symbolic-ref --short -q HEAD)" \
    "$(git -C "$d" rev-parse -q --verify HEAD)" "$(git -C "$d" config --get remote.origin.url)" \
    "$(cd "$d" && cd "$(git rev-parse --git-common-dir)" && pwd)" "$wt"
done"#;
    let out = run_process(
        call,
        remote,
        rookery_api::StartSession {
            kind: rookery_api::SessionKind::Process,
            command: rookery_api::CommandLine::Args(vec![
                "sh".into(),
                "-c".into(),
                SCRIPT.into(),
                "sh".into(),
                folder.into(),
            ]),
            user: "agent".into(),
            cwd: None,
            env: Default::default(),
            input: None,
            input_b64: None,
            title: Some(format!("{}: checkouts", remote.name)),
            timeout: Some(60),
            rows: None,
            cols: None,
        },
        Duration::from_secs(60),
        need,
    )
    .await?;
    let folder = folder.trim_end_matches('/');
    let mut checkouts = vec![];
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 6 {
            continue;
        }
        let some = |text: &str| (!text.is_empty()).then(|| text.to_owned());
        let path = format!("{folder}/{}", fields[0]);
        checkouts.push(Checkout {
            env: crate::uri(&remote.name),
            name: fields[0].to_owned(),
            path,
            branch: some(fields[1]),
            head: some(fields[2]),
            url: some(fields[3]).map(|url| strip_userinfo(&url)),
            worktree_of: (fields[5] == "1").then(|| fields[4].trim_end_matches("/.git").to_owned()),
        });
    }
    checkouts.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(checkouts)
}

/// The checkout at a path, or `NOT_FOUND`.
async fn checkout_at(
    call: &Call,
    remote: &Arc<crate::serve::Remote>,
    folder: &str,
    path: &str,
    need: Grant,
) -> Result<Checkout, RpcError> {
    checkouts_of(call, remote, folder, need)
        .await?
        .into_iter()
        .find(|c| c.path == path)
        .ok_or_else(|| RpcError::not_found(format!("no checkout at {path}")))
}

/// The folder checkouts go in, made if it is not there, as the agent account.
async fn ready_folder(
    call: &Call,
    remote: &Arc<crate::serve::Remote>,
    folder: &str,
) -> Result<String, RpcError> {
    let out = run_process(
        call,
        remote,
        rookery_api::StartSession {
            kind: rookery_api::SessionKind::Process,
            command: rookery_api::CommandLine::Args(vec![
                "mkdir".into(),
                "-p".into(),
                "--".into(),
                folder.into(),
            ]),
            user: "agent".into(),
            cwd: None,
            env: Default::default(),
            input: None,
            input_b64: None,
            title: Some(format!("{}: checkouts folder", remote.name)),
            timeout: Some(30),
            rows: None,
            cols: None,
        },
        Duration::from_secs(30),
        Grant::Use,
    )
    .await?;
    if out.exit_code != Some(0) {
        return Err(RpcError::unavailable(format!(
            "the host could not make {folder}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    resolved(remote, folder).await
}

fn register_workspaces(methods: &mut Methods) {
    methods
        .add("env.exec", |call: Call, p: ExecParams| async move {
            if p.command.is_empty() {
                return Err(RpcError::invalid("command: at least a program"));
            }
            let name = crate::name_of(&p.env).to_owned();
            let remote = linked(&call, &name).await?;
            if p.user != EnvUser::Agent
                && !rookery_facilities::access::holds(&call, &crate::uri(&name), Grant::Manage)
                    .await?
            {
                return Err(RpcError::forbidden(format!(
                    "a command as {} needs manage on {}",
                    p.user.as_str(),
                    crate::uri(&name)
                )));
            }
            let cwd = match &p.cwd {
                Some(cwd) => {
                    let canonical = resolved(&remote, cwd).await?;
                    let roots = host_paths(&remote).await?.roots;
                    if !roots.iter().any(|root| within(&canonical, root)) {
                        return Err(RpcError::forbidden(format!(
                            "{cwd} is not under a folder this host's calls may touch"
                        )));
                    }
                    Some(canonical)
                }
                None => None,
            };
            let asked = p.timeout_ms.map_or(EXEC_TIMEOUT, Duration::from_millis);
            let limit = asked.clamp(Duration::from_millis(100), EXEC_LONGEST);
            let out = run_process(
                &call,
                &remote,
                rookery_api::StartSession {
                    kind: rookery_api::SessionKind::Process,
                    command: rookery_api::CommandLine::Args(p.command.clone()),
                    user: p.user.as_str().into(),
                    cwd,
                    env: p.vars.clone(),
                    input: p.stdin.clone(),
                    input_b64: None,
                    // The program alone: a title is what anyone who may see the host's sessions reads.
                    title: Some(format!("{}: {}", remote.name, p.command[0])),
                    timeout: Some(limit.as_secs().max(1)),
                    rows: None,
                    cols: None,
                },
                limit,
                Grant::Use,
            )
            .await?;
            call.audit(
                "env.exec",
                Some(&crate::uri(&name)),
                // The words may hold something that is not for a log; the program and who ran it say enough.
                json!({"program": p.command[0], "user": p.user.as_str()}),
            )
            .await;
            let (stdout, stderr, cut) = fit(&out.stdout, &out.stderr);
            Ok(ExecResult {
                exit_code: out.exit_code,
                lost: out.lost,
                truncated: out.truncated || cut,
                stdout,
                stderr,
            })
        })
        .summary("Runs a command on a host to its end and answers its output, as the agent account or, with manage, as the operator or root, up to an hour. A result is at most what one message holds, and truncated says when it is not all.")
        .level(level(Grant::Use));
    methods
        .add("env.checkouts", |call: Call, p: EnvRef| async move {
            let remote = linked(&call, crate::name_of(&p.env)).await?;
            let folder = match host_paths(&remote).await?.workspaces {
                Some(folder) => resolved(&remote, &folder).await?,
                None => return Ok(CheckoutList { checkouts: vec![] }),
            };
            Ok(CheckoutList {
                checkouts: checkouts_of(&call, &remote, &folder, Grant::Use).await?,
            })
        })
        .summary("The git checkouts and worktrees under a host's checkouts folder, with their branch, HEAD and remote. A remote's credential is left out.")
        .level(level(Grant::Use));
    methods
        .add("env.clone", |call: Call, p: CheckoutCreate| async move {
            let name = crate::name_of(&p.env).to_owned();
            let remote = linked(&call, &name).await?;
            let folder = ready_folder(&call, &remote, &checkouts_folder(&remote).await?).await?;
            let address = p.repository.trim();
            check_address(address)?;
            let checkout = p.name.clone().unwrap_or_else(|| name_from_url(address));
            check_name(&checkout)?;
            let path = format!("{folder}/{checkout}");
            // A credential in an HTTP address goes in a header from the host's environment: git's
            // argv is what a host's session list shows.
            let (repository, credential) = split_credentials(address);
            let mut configs: Vec<&str> = vec![];
            let mut vars = BTreeMap::new();
            if let Some(credential) = credential {
                vars.insert(
                    "ROOKERY_GIT_AUTH".to_owned(),
                    format!(
                        "Authorization: Basic {}",
                        rookery_api::b64(credential.as_bytes())
                    ),
                );
                configs.push("--config-env=http.extraHeader=ROOKERY_GIT_AUTH");
            }
            let mut args = vec!["clone", "--quiet"];
            if let Some(branch) = &p.branch {
                check_branch(branch)?;
                args.extend(["--branch", branch.as_str()]);
            }
            args.extend(["--", repository.as_str(), path.as_str()]);
            git_with(&call, &remote, &configs, &args, None, GIT_TIMEOUT, vars).await?;
            call.audit(
                "env.clone",
                Some(&crate::uri(&name)),
                json!({"repository": strip_userinfo(address), "path": path}),
            )
            .await;
            checkout_at(&call, &remote, &folder, &path, Grant::Use).await
        })
        .summary(
            "Clones a repository into a host's checkouts folder, and answers the checkout it made.",
        )
        .level(level(Grant::Use));
    methods
        .add("env.worktree", |call: Call, p: WorktreeCreate| async move {
            let name = crate::name_of(&p.env).to_owned();
            let remote = linked(&call, &name).await?;
            let folder = ready_folder(&call, &remote, &checkouts_folder(&remote).await?).await?;
            let checkout = resolved(&remote, &p.checkout).await?;
            if !within(&checkout, &folder) {
                return Err(RpcError::invalid(format!(
                    "{} is not a checkout under {folder}",
                    p.checkout
                )));
            }
            check_branch(&p.branch)?;
            let base = checkout.rsplit('/').next().unwrap_or("checkout").to_owned();
            let worktree = p
                .name
                .clone()
                .unwrap_or_else(|| format!("{base}-{}", p.branch.replace('/', "-")));
            check_name(&worktree)?;
            let path = format!("{folder}/{worktree}");
            let mut args = vec![
                "-C",
                checkout.as_str(),
                "worktree",
                "add",
                "--quiet",
                "-b",
                p.branch.as_str(),
                "--",
                path.as_str(),
            ];
            if let Some(base) = &p.base {
                check_rev(base)?;
                args.push(base.as_str());
            }
            git(&call, &remote, &args, None, GIT_TIMEOUT).await?;
            call.audit(
                "env.worktree",
                Some(&crate::uri(&name)),
                json!({"checkout": checkout, "branch": p.branch, "path": path}),
            )
            .await;
            checkout_at(&call, &remote, &folder, &path, Grant::Use).await
        })
        .summary(
            "Makes a git worktree of a checkout, on a new branch, in a host's checkouts folder.",
        )
        .level(level(Grant::Use));
    methods
        .add(
            "env.removeCheckout",
            |call: Call, p: CheckoutRemove| async move {
                let name = crate::name_of(&p.env).to_owned();
                let remote = linked(&call, &name).await?;
                let folder = resolved(&remote, &checkouts_folder(&remote).await?).await?;
                let path = resolved(&remote, &p.path).await?;
                if !within(&path, &folder) {
                    return Err(RpcError::invalid(format!(
                        "{} is not a checkout under {folder}",
                        p.path
                    )));
                }
                let found = checkout_at(&call, &remote, &folder, &path, Grant::Use).await?;
                match &found.worktree_of {
                    Some(main) => {
                        let mut args = vec!["-C", main.as_str(), "worktree", "remove"];
                        if p.force {
                            args.push("--force");
                        }
                        args.extend(["--", path.as_str()]);
                        git(&call, &remote, &args, None, GIT_TIMEOUT).await?;
                    }
                    None => {
                        if !p.force {
                            let status = git(
                                &call,
                                &remote,
                                &["-C", path.as_str(), "status", "--porcelain", "--ignored", "--"],
                                None,
                                GIT_TIMEOUT,
                            )
                            .await?;
                            if !status.stdout.iter().all(u8::is_ascii_whitespace) {
                                return Err(RpcError::conflict(format!(
                                    "{path} has changes; pass force to remove it anyway"
                                )));
                            }
                        }
                        let target = format!("{}&recursive=true", files_target(&path));
                        let answer = link_of(&remote)?
                            .send("DELETE", &target, &[], &[])
                            .await
                            .map_err(api::Fail::from)?;
                        if answer.status >= 400 && answer.status != 404 {
                            return Err(refused(&path, answer.status, &answer.body));
                        }
                    }
                }
                call.audit(
                    "env.removeCheckout",
                    Some(&crate::uri(&name)),
                    json!({"path": path}),
                )
                .await;
                Ok(Empty {})
            },
        )
        .summary("Removes a checkout, or a worktree through `git worktree remove`, from a host's checkouts folder. Without force, one with changes is a CONFLICT, and any caller with use may force it.")
        .level(level(Grant::Use));
}

pub fn register(methods: &mut Methods) {
    methods
        .add("env.list", |call: Call, _: Empty| async move {
            let state = serve(&call)?;
            let mut environments = vec![];
            for host in api::all_infos(&state).await {
                if let Some(info) = visible(&call, &host).await? {
                    environments.push(info);
                }
            }
            Ok(EnvList { environments })
        })
        .summary("The hosts the caller may see, as environments: their state, what each supports and the caller's level on it.")
        .level(Level::Own);
    methods
        .add("env.get", |call: Call, p: EnvRef| async move {
            let state = serve(&call)?;
            let host = api::host_info(&state, crate::name_of(&p.env)).await?;
            visible(&call, &host).await?.ok_or_else(|| {
                RpcError::forbidden(format!("this needs view on {}", crate::uri(&host.name)))
            })
        })
        .summary("One host as an environment.")
        .level(level(Grant::View));
    methods
        .add("env.presence", |call: Call, p: EnvRef| async move {
            let name = crate::name_of(&p.env);
            let users = call
                .store()
                .terminals()
                .presence(name)
                .await?
                .into_iter()
                .map(|(id, name, count, since)| PresenceUser {
                    id: id.to_string(),
                    name,
                    terminals: count as u32,
                    since: since.to_rfc3339(),
                })
                .collect();
            Ok(EnvPresence { users })
        })
        .summary(
            "Who has terminals running on a host, and how many: never the terminals themselves.",
        )
        .level(level(Grant::View));
    methods
        .add("env.readFile", |call: Call, p: FilePath| async move {
            let remote = linked(&call, &p.env).await?;
            let bytes = read_bytes(link_of(&remote)?, &p.path)
                .await?
                .ok_or_else(|| RpcError::not_found(format!("no file {}", p.path)))?;
            Ok(FileData {
                size: bytes.len() as u64,
                sha256: sha256(&bytes),
                base64: rookery_api::b64(&bytes),
            })
        })
        .summary("A file on a host inside its roots, as the agent account, in base64 with its SHA-256. Up to 8 MiB.")
        .level(level(Grant::Use));
    methods
        .add("env.writeFile", |call: Call, p: EnvFileWrite| async move {
            let bytes = rookery_api::unb64(&p.base64)
                .ok_or_else(|| RpcError::invalid("base64: not base64"))?;
            if bytes.len() > MAX_FILE {
                return Err(RpcError::invalid(format!("a write is at most {} MiB", MAX_FILE >> 20)));
            }
            let remote = linked(&call, &p.env).await?;
            let _held = lock_path(&call, &remote, &p.path).await?;
            let link = link_of(&remote)?;
            if let Some(wanted) = &p.if_match {
                let current = read_bytes(link, &p.path).await?.map(|b| sha256(&b));
                if current.as_deref() != Some(wanted.as_str()) {
                    return Err(RpcError::conflict(format!("{} changed since it was read", p.path))
                        .with_details(json!({"current": current})));
                }
            }
            let overwrite = p.overwrite || p.if_match.is_some();
            let target = format!(
                "{}{}",
                files_target(&p.path),
                if overwrite { "&overwrite=true" } else { "" }
            );
            let answer = link
                .send("PUT", &target, &[("content-type", "application/octet-stream")], &bytes)
                .await
                .map_err(api::Fail::from)?;
            if answer.status >= 400 {
                return Err(refused(&p.path, answer.status, &answer.body));
            }
            call.audit("env.writeFile", Some(&crate::uri(&remote.name)), json!({"path": p.path})).await;
            Ok(FileWritten {
                sha256: sha256(&bytes),
                size: bytes.len() as u64,
            })
        })
        .summary("Writes a file on a host inside its roots, as the agent account. With ifMatch only while the file is at that SHA-256.")
        .level(level(Grant::Use));
    methods
        .add("env.deleteFile", |call: Call, p: FileDelete| async move {
            let remote = linked(&call, &p.env).await?;
            let _held = lock_path(&call, &remote, &p.path).await?;
            let target = format!(
                "{}{}",
                files_target(&p.path),
                if p.recursive { "&recursive=true" } else { "" }
            );
            let answer = link_of(&remote)?
                .send("DELETE", &target, &[], &[])
                .await
                .map_err(api::Fail::from)?;
            if answer.status >= 400 {
                return Err(refused(&p.path, answer.status, &answer.body));
            }
            call.audit("env.deleteFile", Some(&crate::uri(&remote.name)), json!({"path": p.path})).await;
            Ok(Empty {})
        })
        .summary("Removes a file, or a folder with recursive, on a host inside its roots, as the agent account.")
        .level(level(Grant::Use));
    methods
        .add("env.listDir", |call: Call, p: FilePath| async move {
            let remote = linked(&call, &p.env).await?;
            let found = listing(link_of(&remote)?, &p.path)
                .await?
                .ok_or_else(|| RpcError::not_found(format!("no folder {}", p.path)))?;
            Ok(DirListing {
                path: found.path,
                entries: found.entries.into_iter().map(entry_of).collect(),
            })
        })
        .summary("What a folder on a host holds, inside its roots.")
        .level(level(Grant::Use));
    methods
        .add("env.stat", |call: Call, p: FilePath| async move {
            let remote = linked(&call, &p.env).await?;
            let path = p.path.trim_end_matches('/');
            let (parent, name) = match path.rsplit_once('/') {
                Some(("", name)) => ("/", name),
                Some((parent, name)) => (parent, name),
                None => return Err(RpcError::invalid("path is absolute")),
            };
            let entry = match listing(link_of(&remote)?, parent).await {
                Ok(Some(found)) => found
                    .entries
                    .into_iter()
                    .find(|e| e.name == name)
                    .map(entry_of),
                Ok(None) => None,
                // The parent is a root's own parent, which the controller does not list.
                Err(e) if e.error_code() == rookery_proto::ErrorCode::Forbidden => {
                    listing(link_of(&remote)?, path).await?.map(|_| DirEntry {
                        name: name.to_owned(),
                        kind: FileKind::Dir,
                        size: 0,
                        modified: None,
                    })
                }
                Err(e) => return Err(e),
            };
            Ok(FileStat {
                path: p.path,
                entry,
            })
        })
        .summary("What a path on a host is, inside its roots; no entry when nothing is there.")
        .level(level(Grant::Use));
    methods
        .add("env.connect", |call: Call, p: Connect| async move {
            if p.port == 0 {
                return Err(RpcError::invalid("port: from 1 to 65535"));
            }
            let remote = linked(&call, &p.env).await?;
            let upstream = link_of(&remote)?
                .stream(&format!("/v1/ports/{}", p.port))
                .await
                .map_err(api::Fail::from)?;
            let stream = call.conn()?.streams().open(Initiator::Peer)?;
            let id = stream.id();
            let access =
                crate::access_watch(&call, crate::uri(&remote.name), Grant::Use).subscribe();
            tokio::spawn(crate::serve::pipe(
                upstream,
                stream,
                crate::serve::Carry::Bytes,
                Some(access),
            ));
            call.audit(
                "env.connect",
                Some(&crate::uri(&remote.name)),
                json!({"port": p.port}),
            )
            .await;
            Ok(StreamOpened {
                stream: id,
                offset: None,
            })
        })
        .summary("A byte stream to a TCP port on a host's loopback.")
        .stream()
        .level(level(Grant::Use));
    methods
        .add("env.portUrl", |call: Call, p: PortRef| async move {
            let name = crate::name_of(&p.env).to_owned();
            // The host must exist, and reach ports.
            linked(&call, &name).await?;
            let user = call.user_id()?;
            crate::proxy::port_url(&call.kernel, user, &name, p.port, p.path.as_deref().unwrap_or("/"))
        })
        .summary("An address that opens a port of a host in a browser, on the ports' own origin, signing in to that port alone. It works once, within a minute.")
        .level(level(Grant::Use));
    register_workspaces(methods);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_loses_its_credential_before_it_is_shown() {
        assert_eq!(
            strip_userinfo("https://ada:token@example.test/acme/widgets.git"),
            "https://example.test/acme/widgets.git"
        );
        assert_eq!(
            strip_userinfo("https://token@example.test/acme/widgets.git"),
            "https://example.test/acme/widgets.git"
        );
        assert_eq!(
            strip_userinfo("ssh://git@example.test/acme/widgets.git"),
            "ssh://example.test/acme/widgets.git"
        );
        // A folder, and an SSH address written as the command line writes it, have no scheme.
        assert_eq!(strip_userinfo("/srv/repos/widgets"), "/srv/repos/widgets");
        assert_eq!(
            strip_userinfo("git@example.test:acme/widgets.git"),
            "git@example.test:acme/widgets.git"
        );
    }

    #[test]
    fn an_http_credential_goes_in_a_header_not_in_the_address() {
        assert_eq!(
            split_credentials("https://ada:token@example.test/acme/widgets.git"),
            (
                "https://example.test/acme/widgets.git".to_owned(),
                Some("ada:token".to_owned())
            )
        );
        assert_eq!(
            split_credentials("https://token@example.test/acme/widgets.git"),
            (
                "https://example.test/acme/widgets.git".to_owned(),
                Some("token:".to_owned())
            )
        );
        assert_eq!(
            split_credentials("ssh://git@example.test/acme/widgets.git"),
            ("ssh://git@example.test/acme/widgets.git".to_owned(), None)
        );
        assert_eq!(
            split_credentials("/srv/repos/widgets"),
            ("/srv/repos/widgets".to_owned(), None)
        );
    }

    #[test]
    fn a_transport_that_runs_a_command_is_refused() {
        assert_eq!(
            code(check_address("ext::sh -c 'echo evil'")),
            Some(rookery_proto::ErrorCode::Invalid)
        );
        assert_eq!(
            code(check_address("fd::7")),
            Some(rookery_proto::ErrorCode::Invalid)
        );
        for address in [
            "https://example.test/acme/widgets.git",
            "ssh://git@example.test/acme/widgets.git",
            "git@example.test:acme/widgets.git",
            "/srv/repos/widgets",
        ] {
            assert_eq!(code(check_address(address)), None, "{address}");
        }
    }

    fn code(result: Result<(), RpcError>) -> Option<rookery_proto::ErrorCode> {
        result.err().map(|error| error.error_code())
    }

    #[test]
    fn names_are_folders_and_branches_are_refs() {
        assert!(check_name("widgets-1.2_x").is_ok());
        for name in ["", ".", "..", "-escape", "../escape", "a/b", "a b"] {
            assert!(check_name(name).is_err(), "{name}");
        }
        assert!(check_branch("feature/one").is_ok());
        for branch in ["", "-main", "a..b", "a b", "a~1", "a.lock"] {
            assert!(check_branch(branch).is_err(), "{branch}");
        }
        assert!(check_rev("origin/main").is_ok());
        assert!(check_rev("HEAD~2").is_ok());
        assert!(check_rev("-x").is_err());
    }

    #[test]
    fn a_result_is_cut_to_what_a_message_holds() {
        let (stdout, stderr, cut) = fit(&vec![b'a'; MAX_COLLECT], b"");
        assert!(cut);
        assert!(stderr.is_empty());
        assert!(encoded(&stdout) <= MAX_RESULT, "{}", encoded(&stdout));
        let (stdout, stderr, cut) = fit(b"one\n", b"two\n");
        assert_eq!(
            (stdout.as_str(), stderr.as_str(), cut),
            ("one\n", "two\n", false)
        );
        // A stream that fills the budget leaves no room for the other, and that is not a cut.
        let full = vec![b'a'; MAX_RESULT - 4];
        let (stdout, stderr, cut) = fit(&full, b"");
        assert_eq!(
            (stdout.len(), stderr.as_str(), cut),
            (full.len(), "", false)
        );
    }
}
