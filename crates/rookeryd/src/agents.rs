//! The controller brokers the server's connection to a fixed local Parrot socket.

use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{
    State as AppState,
    ws::{WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
};
use axum::response::Response;
use tokio::net::UnixStream;

use crate::error::{E, R};
use crate::{Mode, State};

fn endpoint(state: &State) -> (PathBuf, u32) {
    let dir = match state.mode {
        Mode::System => PathBuf::from(rookery_api::RUN_DIR),
        Mode::Single => state.state_dir.join(".."),
    };
    let uid = match state.mode {
        Mode::System => 0,
        // SAFETY: geteuid only reads this process's effective UID.
        Mode::Single => unsafe { libc::geteuid() },
    };
    (dir.join(rookery_api::PARROT_SOCKET), uid)
}

/// Whether `dir` is a real directory only its owner can write to. The socket
/// is only reached through it, so another owner that could rename it could
/// put another socket behind the same path.
fn private_dir(dir: &std::path::Path, uid: u32) -> bool {
    std::fs::symlink_metadata(dir)
        .is_ok_and(|meta| meta.is_dir() && meta.uid() == uid && meta.mode() & 0o022 == 0)
}

fn private_socket(path: &std::path::Path, uid: u32) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| {
        meta.file_type().is_socket() && meta.uid() == uid && meta.mode() & 0o777 == 0o600
    })
}

/// Whether the broker's folder and socket are the ones expected, in that
/// order, before either is used or connected to.
fn private_endpoint(path: &std::path::Path, uid: u32) -> bool {
    path.parent().is_some_and(|dir| private_dir(dir, uid)) && private_socket(path, uid)
}

pub(crate) fn available(state: &State) -> bool {
    let (path, uid) = endpoint(state);
    private_endpoint(&path, uid)
}

pub(crate) async fn connect(
    AppState(state): AppState<Arc<State>>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> R<Response> {
    let upgrade = upgrade.map_err(|_| E::bad("Parrot is opened only as a WebSocket"))?;
    let (path, uid) = endpoint(&state);
    if !private_endpoint(&path, uid) {
        return Err(E::unavailable(
            "the host has no private Parrot broker socket",
        ));
    }
    let socket = tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(path))
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or_else(|| E::unavailable("the Parrot broker does not answer"))?;
    if !socket.peer_cred().is_ok_and(|peer| peer.uid() == uid) {
        return Err(E::unavailable("the Parrot broker has an unexpected owner"));
    }
    Ok(upgrade.on_upgrade(move |ws| crate::ports::bridge(ws, socket, false)))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn the_broker_socket_is_reached_through_a_private_folder_of_its_own() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("run");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.join(rookery_api::PARROT_SOCKET);
        let _socket = std::os::unix::net::UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        // SAFETY: geteuid only reads this process's effective UID.
        let uid = unsafe { libc::geteuid() };
        assert!(private_endpoint(&path, uid));

        // A folder another account may write to could have the socket renamed under it.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(!private_endpoint(&path, uid));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        // A socket others may open is not the broker's private one.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(!private_endpoint(&path, uid));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        // A folder that is a link is not the real one, wherever it points.
        let real = root.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::rename(&dir, root.path().join("moved")).unwrap();
        std::os::unix::fs::symlink(&real, &dir).unwrap();
        assert!(!private_endpoint(
            &dir.join(rookery_api::PARROT_SOCKET),
            uid
        ));
    }
}
