//! Hosts as environments: `env.*`, a projection of the saved hosts for clients that work on them.
//!
//! `view` on `env://NAME` sees a host, its state and who works there. `use` reaches its files inside
//! its roots and its loopback ports, as the agent account. Nothing here writes as another account.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rookery_core::host_type::Capability;
use rookery_kernel::streams::Initiator;
use rookery_kernel::{Call, Empty, Grant, Level, Methods, RpcError};
use rookery_proto::env::*;
use rookery_proto::hosts::HostInfo;
use rookery_proto::stream::StreamOpened;
use serde_json::json;
use sha2::Digest;

use crate::serve::{Serve, api};

/// The largest file `env.readFile` and `env.writeFile` carry, as a call's message is at most 12 MiB.
pub const MAX_FILE: usize = 8 * 1024 * 1024;

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
        checkouts: false,
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
    EnvInfo {
        name: host.name.clone(),
        uri: crate::uri(&host.name),
        kind: kind_of(host.host_type),
        state,
        owner,
        level,
        os: None,
        version: host.version.clone(),
        supports: supports(host.host_type),
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
    let link = link_of(remote)?;
    let answer = link
        .send(
            "GET",
            &format!("{}&resolve=true", files_target(path)),
            &[],
            &[],
        )
        .await
        .map_err(api::Fail::from)?;
    let canonical = if answer.status == 200 {
        let parsed = serde_json::from_slice::<serde_json::Value>(&answer.body).ok();
        match parsed
            .as_ref()
            .filter(|v| v.get("entries").is_none())
            .and_then(|v| v["path"].as_str())
        {
            Some(canon) => canon.to_owned(),
            None => lexical_clean(path),
        }
    } else if answer.status == 404 {
        lexical_clean(path)
    } else {
        return Err(refused(path, answer.status, &answer.body));
    };
    let lock = call
        .kernel
        .facility::<WriteLocks>()
        .of(&remote.name, &canonical);
    Ok(lock.lock_owned().await)
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
}
