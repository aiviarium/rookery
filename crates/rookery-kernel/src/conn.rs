//! Connections on `/ws`: JSON-RPC in text frames, stream data in binary frames.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message as WsMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};
use rookery_proto::{Message, Notification, Response};
use serde::Serialize;
use tokio::sync::mpsc;
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

pub type OutTx = mpsc::UnboundedSender<Outbound>;

/// The close code that says the connection's session or token was revoked. A client shows its sign-in
/// instead of connecting again.
pub const CLOSE_REVOKED: u16 = 4401;

/// Puts a notification on a socket's queue. A closed socket drops it.
pub fn send_notification(out: &OutTx, method: &str, params: impl Serialize) {
    let params = serde_json::to_value(params).expect("notification params serialize");
    let _ = out.send(Outbound::Text(
        Message::Notification(Notification::new(method, params)).to_text(),
    ));
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
pub async fn serve(kernel: Kernel, socket: WebSocket, caller: Caller) {
    let (mut sink, mut source) = socket.split();
    let (out, mut rx) = mpsc::unbounded_channel::<Outbound>();
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

    let (close_tx, mut close_rx) = mpsc::channel::<()>(1);
    let mut writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(Duration::from_secs(20));
        ping.tick().await;
        loop {
            let message = tokio::select! {
                next = rx.recv() => match next {
                    Some(Outbound::Text(text)) => WsMessage::Text(text.into()),
                    Some(Outbound::Binary(bytes)) => WsMessage::Binary(bytes.into()),
                    None => break,
                },
                _ = ping.tick() => WsMessage::Ping(Vec::new().into()),
                Some(()) = close_rx.recv() => {
                    let frame = CloseFrame { code: CLOSE_REVOKED, reason: "the session ended".into() };
                    let _ = sink.send(WsMessage::Close(Some(frame))).await;
                    break;
                }
            };
            if sink.send(message).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let mut revoked = false;
    loop {
        let message = tokio::select! {
            next = source.next() => match next {
                Some(Ok(message)) => message,
                _ => break,
            },
            _ = conn.0.revoked.cancelled() => {
                revoked = true;
                break;
            }
            _ = conn.0.refresh.cancelled() => break,
            _ = kernel.shutdown.cancelled() => break,
        };
        match message {
            WsMessage::Text(text) => handle_text(&kernel, &conn, &out, text.as_str()),
            WsMessage::Binary(data) => conn.streams().deliver(&data),
            WsMessage::Close(_) => break,
            WsMessage::Ping(_) | WsMessage::Pong(_) => {}
        }
    }
    if revoked {
        // Say why, so the client shows its sign-in instead of connecting again in a loop.
        let _ = close_tx.send(()).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), &mut writer).await;
    }
    kernel.conns.map.write().unwrap().remove(&id);
    conn.streams().close_all();
    kernel.methods.connection_closed(&kernel, &conn);
    drop(out);
    drop(conn);
    writer.abort();
}

fn handle_text(kernel: &Kernel, conn: &ConnHandle, out: &OutTx, text: &str) {
    let message = match Message::parse(text) {
        Ok(message) => message,
        Err(refused) => {
            let _ = out.send(Outbound::Text(Message::Response(refused).to_text()));
            return;
        }
    };
    let call = Call::new(kernel.clone(), conn.caller().clone(), Some(conn.clone()));
    match message {
        Message::Request(request) => {
            let done = kernel
                .methods
                .dispatch(call, &request.method, request.params);
            let out = out.clone();
            tokio::spawn(async move {
                let response = Response::from_result(request.id, done.await);
                let _ = out.send(Outbound::Text(Message::Response(response).to_text()));
            });
        }
        Message::Notification(notification) => {
            let done = kernel
                .methods
                .dispatch(call, &notification.method, notification.params);
            tokio::spawn(async move {
                if let Err(e) = done.await {
                    tracing::debug!(method = %notification.method, error = %e, "a notification failed");
                }
            });
        }
        // The server sends clients no requests, so a response has nothing to answer.
        Message::Response(_) => {}
    }
}
