//! Connections on `/ws`: JSON-RPC in text frames, stream data in binary frames.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message as WsMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};
use rookery_proto::{Message, Notification, Response};
use serde::Serialize;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::kernel::Kernel;
use crate::methods::{Call, Caller};
use crate::streams::Streams;

/// A frame waiting to go out on a socket.
#[derive(Debug)]
pub enum Outbound {
    Text(String),
    Binary(Vec<u8>),
}

/// Replies and notifications that may wait for a slow socket.
const MAX_OUTBOUND_FRAMES: usize = 32;
/// Stream data that may wait beside them. Data waits for room instead of ending the connection, and its
/// own share keeps it from taking the room of replies and notifications.
const MAX_OUTBOUND_BINARY: usize = 32;
const MAX_OUTBOUND_MESSAGE: usize = 2 * 1024 * 1024;
const MAX_PENDING_CALLS: usize = 16;
const MAX_PENDING_CALL_BYTES: usize = 12 * 1024 * 1024;

#[derive(Clone)]
pub struct OutTx {
    tx: mpsc::Sender<Outbound>,
    /// Binary frames that may be queued: taken by a sender, given back by the socket's writer.
    binary: Arc<Semaphore>,
    overflow: CancellationToken,
}

impl OutTx {
    pub(crate) fn channel() -> (Self, mpsc::Receiver<Outbound>) {
        let (tx, rx) = mpsc::channel(MAX_OUTBOUND_FRAMES + MAX_OUTBOUND_BINARY);
        (
            Self {
                tx,
                binary: Arc::new(Semaphore::new(MAX_OUTBOUND_BINARY)),
                overflow: CancellationToken::new(),
            },
            rx,
        )
    }

    /// A slow peer must reconnect if it cannot keep up with replies or control notifications.
    fn send_text(&self, text: String) {
        if text.len() > MAX_OUTBOUND_MESSAGE || self.tx.try_send(Outbound::Text(text)).is_err() {
            self.overflow.cancel();
        }
    }

    pub(crate) async fn send_binary(&self, bytes: Vec<u8>) -> Result<(), crate::RpcError> {
        let closed = || crate::RpcError::unavailable("the connection is closed");
        self.binary.acquire().await.map_err(|_| closed())?.forget();
        self.tx
            .send(Outbound::Binary(bytes))
            .await
            .map_err(|_| closed())
    }

    /// The receiver took a binary frame off the queue, which makes room for another, as the socket's
    /// writer does.
    #[cfg(test)]
    fn took_binary(&self) {
        self.binary.add_permits(1);
    }
}

/// The close code that says the connection's session or token was revoked. A client shows its sign-in
/// instead of connecting again.
pub const CLOSE_REVOKED: u16 = 4401;

/// Puts a notification on a socket's queue. A closed socket drops it.
pub fn send_notification(out: &OutTx, method: &str, params: impl Serialize) {
    let params = serde_json::to_value(params).expect("notification params serialize");
    out.send_text(Message::Notification(Notification::new(method, params)).to_text());
}

/// A live connection.
#[derive(Clone)]
pub struct ConnHandle(Arc<ConnInner>);

struct ConnInner {
    id: u64,
    caller: Caller,
    out: OutTx,
    streams: Streams,
    /// Cancelled when the credential the connection signed in with is revoked.
    revoked: CancellationToken,
    /// Cancelled when what the connection signed in as changed, such as the user's role: it closes, and
    /// the client connects again as the person now is.
    refresh: CancellationToken,
}

impl ConnHandle {
    pub fn id(&self) -> u64 {
        self.0.id
    }
    pub fn caller(&self) -> &Caller {
        &self.0.caller
    }
    pub fn streams(&self) -> &Streams {
        &self.0.streams
    }
    pub fn notify(&self, method: &str, params: impl Serialize) {
        send_notification(&self.0.out, method, params);
    }
    fn session(&self) -> Option<Uuid> {
        match &self.0.caller {
            Caller::User { session, .. } => *session,
            _ => None,
        }
    }
    fn token(&self) -> Option<Uuid> {
        match &self.0.caller {
            Caller::User { token, .. } => *token,
            _ => None,
        }
    }
    fn user_id(&self) -> Option<&str> {
        self.0.caller.user().map(|u| u.id.as_str())
    }
}

impl ConnHandle {
    /// A connection with no socket: what the server sends on it arrives on the receiver. For tests, and
    /// for code inside the server that calls methods as a client would.
    pub fn detached(kernel: &Kernel, caller: Caller) -> (ConnHandle, mpsc::Receiver<Outbound>) {
        let (out, rx) = OutTx::channel();
        let id = kernel.conns.next.fetch_add(1, Ordering::Relaxed) + 1;
        let conn = ConnHandle(Arc::new(ConnInner {
            id,
            caller,
            out: out.clone(),
            streams: Streams::new(out),
            revoked: CancellationToken::new(),
            refresh: CancellationToken::new(),
        }));
        kernel.conns.map.write().unwrap().insert(id, conn.clone());
        (conn, rx)
    }

    /// Ends a detached connection, as a socket's end ends one.
    pub fn close_detached(&self, kernel: &Kernel) {
        kernel.conns.map.write().unwrap().remove(&self.0.id);
        self.streams().close_all();
        kernel.methods.connection_closed(kernel, self);
    }

    /// Whether the connection was told to end: its credential was revoked or its user changed.
    pub fn ended(&self) -> bool {
        self.0.revoked.is_cancelled() || self.0.refresh.is_cancelled()
    }
}

#[derive(Default)]
pub struct Connections {
    map: RwLock<HashMap<u64, ConnHandle>>,
    next: AtomicU64,
}

impl Connections {
    pub fn all(&self) -> Vec<ConnHandle> {
        self.map.read().unwrap().values().cloned().collect()
    }

    pub fn count(&self) -> usize {
        self.map.read().unwrap().len()
    }

    /// Sends a notification to every connection of a user.
    pub fn notify_user(&self, user: &str, method: &str, params: impl Serialize) {
        let params = serde_json::to_value(params).expect("notification params serialize");
        for conn in self.map.read().unwrap().values() {
            if conn.user_id() == Some(user) {
                conn.notify(method, &params);
            }
        }
    }

    /// Ends the connections of a device session that was revoked.
    pub fn revoke_session(&self, session: Uuid) {
        for conn in self.map.read().unwrap().values() {
            if conn.session() == Some(session) {
                conn.0.revoked.cancel();
            }
        }
    }

    /// Ends the connections of an access token that was revoked.
    pub fn revoke_token(&self, token: Uuid) {
        for conn in self.map.read().unwrap().values() {
            if conn.token() == Some(token) {
                conn.0.revoked.cancel();
            }
        }
    }

    /// Ends every connection of a user who was disabled.
    pub fn revoke_user(&self, user: &str) {
        for conn in self.map.read().unwrap().values() {
            if conn.user_id() == Some(user) {
                conn.0.revoked.cancel();
            }
        }
    }

    /// Closes the connections of a user whose role changed. The client connects again with the role the
    /// person has now, so what a connection says about its user is never older than the connection.
    pub fn refresh_user(&self, user: &str) {
        for conn in self.map.read().unwrap().values() {
            if conn.user_id() == Some(user) {
                conn.0.refresh.cancel();
            }
        }
    }
}

/// Serves one connection until it closes.
pub async fn serve(
    kernel: Kernel,
    mut socket: WebSocket,
    caller: Caller,
    credential: Option<String>,
) {
    let (out, mut rx) = OutTx::channel();
    let id = kernel.conns.next.fetch_add(1, Ordering::Relaxed) + 1;
    let conn = ConnHandle(Arc::new(ConnInner {
        id,
        caller,
        out: out.clone(),
        streams: Streams::new(out.clone()),
        revoked: CancellationToken::new(),
        refresh: CancellationToken::new(),
    }));
    kernel.conns.map.write().unwrap().insert(id, conn.clone());

    // Check again after registering so that a concurrent revoke or role change cannot miss the socket.
    let expiry = if let Some(credential) = credential {
        match crate::auth::authenticate_with_expiry(&kernel, &credential).await {
            Some((caller, expiry)) if caller == *conn.caller() => expiry,
            _ => {
                kernel.conns.map.write().unwrap().remove(&id);
                let _ = socket.send(revoked_frame()).await;
                return;
            }
        }
    } else {
        None
    };
    let expires = async {
        match expiry {
            Some(duration) => tokio::time::sleep(duration).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(expires);

    let (mut sink, mut source) = socket.split();
    let (close_tx, mut close_rx) = mpsc::channel::<()>(1);
    let binary_room = out.binary.clone();
    let mut writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(Duration::from_secs(20));
        ping.tick().await;
        loop {
            let message = tokio::select! {
                biased;
                Some(()) = close_rx.recv() => {
                    let _ = sink.send(revoked_frame()).await;
                    break;
                }
                next = rx.recv() => match next {
                    Some(Outbound::Text(text)) => WsMessage::Text(text.into()),
                    Some(Outbound::Binary(bytes)) => {
                        binary_room.add_permits(1);
                        WsMessage::Binary(bytes.into())
                    }
                    None => break,
                },
                _ = ping.tick() => WsMessage::Ping(Vec::new().into()),
            };
            if sink.send(message).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let mut revoked = false;
    let mut calls = JoinSet::new();
    let pending_bytes = Arc::new(Semaphore::new(MAX_PENDING_CALL_BYTES));
    loop {
        let message = tokio::select! {
            biased;
            _ = conn.0.revoked.cancelled() => {
                revoked = true;
                break;
            }
            _ = &mut expires => {
                revoked = true;
                break;
            }
            _ = conn.0.refresh.cancelled() => break,
            _ = kernel.shutdown.cancelled() => break,
            _ = out.overflow.cancelled() => break,
            _ = &mut writer => break,
            _ = calls.join_next(), if !calls.is_empty() => continue,
            next = source.next() => match next {
                Some(Ok(message)) => message,
                _ => break,
            },
        };
        match message {
            WsMessage::Text(text) => {
                handle_text(
                    &kernel,
                    &conn,
                    &out,
                    &mut calls,
                    &pending_bytes,
                    text.as_str(),
                )
                .await
            }
            WsMessage::Binary(data) => conn.streams().deliver(&data),
            WsMessage::Close(_) => break,
            WsMessage::Ping(_) | WsMessage::Pong(_) => {}
        }
    }
    calls.shutdown().await;
    kernel.conns.map.write().unwrap().remove(&id);
    conn.streams().close_all();
    if revoked {
        // Say why, so the client shows its sign-in instead of connecting again in a loop.
        let _ = close_tx.send(()).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), &mut writer).await;
    }
    kernel.methods.connection_closed(&kernel, &conn);
    drop(out);
    drop(conn);
    writer.abort();
}

fn revoked_frame() -> WsMessage {
    WsMessage::Close(Some(CloseFrame {
        code: CLOSE_REVOKED,
        reason: "the session ended".into(),
    }))
}

async fn handle_text(
    kernel: &Kernel,
    conn: &ConnHandle,
    out: &OutTx,
    calls: &mut JoinSet<()>,
    pending_bytes: &Arc<Semaphore>,
    text: &str,
) {
    let (id, method, params) = match Message::parse(text) {
        Ok(Message::Request(r)) => (Some(r.id), r.method, r.params),
        Ok(Message::Notification(n)) => (None, n.method, n.params),
        Ok(Message::Response(_)) => return,
        Err(refused) => {
            out.send_text(Message::Response(refused).to_text());
            return;
        }
    };
    // Stream control is in wire order relative to binary frames and does not wait for a call slot.
    let control = matches!(
        method.as_str(),
        "streams.credit" | "streams.eof" | "streams.close"
    );
    let permit = (!control && calls.len() < MAX_PENDING_CALLS)
        .then(|| {
            pending_bytes
                .clone()
                .try_acquire_many_owned(text.len() as u32)
                .ok()
        })
        .flatten();
    if !control && permit.is_none() {
        if let Some(id) = id {
            let response = Response::from_result(
                id,
                Err(crate::RpcError::unavailable("too many calls in flight")),
            );
            out.send_text(Message::Response(response).to_text());
        }
        return;
    }
    let call = Call::new(kernel.clone(), conn.caller().clone(), Some(conn.clone()));
    let done = kernel.methods.dispatch(call, &method, params);
    let out = out.clone();
    let finish = async move {
        let _permit = permit;
        let result = done.await;
        if let Some(id) = id {
            out.send_text(Message::Response(Response::from_result(id, result)).to_text());
        } else if let Err(e) = result {
            tracing::debug!(method = %method, error = %e, "a notification failed");
        }
    };
    if control {
        finish.await;
    } else {
        calls.spawn(finish);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slow_socket_bounds_its_outbound_queue() {
        let (out, rx) = OutTx::channel();
        for _ in 0..MAX_OUTBOUND_FRAMES + MAX_OUTBOUND_BINARY {
            out.send_text("message".into());
        }
        assert!(!out.overflow.is_cancelled());
        out.send_text("message".into());
        assert!(out.overflow.is_cancelled());
        assert_eq!(rx.len(), MAX_OUTBOUND_FRAMES + MAX_OUTBOUND_BINARY);
    }

    #[tokio::test]
    async fn stream_data_waiting_for_a_socket_leaves_room_for_replies() {
        let (out, mut rx) = OutTx::channel();
        // A writer within its credit queues more data than the socket takes for now.
        let writer = {
            let out = out.clone();
            tokio::spawn(async move {
                for _ in 0..MAX_OUTBOUND_BINARY * 2 {
                    out.send_binary(vec![0; 16]).await.unwrap();
                }
            })
        };
        while rx.len() < MAX_OUTBOUND_BINARY {
            tokio::task::yield_now().await;
        }
        tokio::task::yield_now().await;
        assert_eq!(rx.len(), MAX_OUTBOUND_BINARY, "data waits for its share");
        // A reply and notifications still fit, and the connection stays.
        for _ in 0..MAX_OUTBOUND_FRAMES {
            out.send_text("reply".into());
        }
        assert!(!out.overflow.is_cancelled());
        // As the socket takes frames, the data that waited goes out.
        let mut binary = 0;
        while binary < MAX_OUTBOUND_BINARY * 2 {
            if let Some(Outbound::Binary(_)) = rx.recv().await {
                out.took_binary();
                binary += 1;
            }
        }
        writer.await.unwrap();
    }
}
