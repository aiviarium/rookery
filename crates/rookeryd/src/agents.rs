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

fn private_socket(path: &std::path::Path, uid: u32) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| {
        meta.file_type().is_socket() && meta.uid() == uid && meta.mode() & 0o777 == 0o600
    })
}

pub(crate) fn available(state: &State) -> bool {
    let (path, uid) = endpoint(state);
    private_socket(&path, uid)
}

pub(crate) async fn connect(
    AppState(state): AppState<Arc<State>>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> R<Response> {
    let upgrade = upgrade.map_err(|_| E::bad("Parrot is opened only as a WebSocket"))?;
    let (path, uid) = endpoint(&state);
    if !private_socket(&path, uid) {
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
