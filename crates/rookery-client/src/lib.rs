//! A Rust client of `rookery serve`: JSON-RPC 2.0 over `/ws` with its stream channels, over TCP, TLS
//! or the server's Unix socket, and one call over `POST /rpc`. The CLI does everything through it. No
//! server crate reaches it, so it builds wherever the CLI does.
//!
//! ```text
//! let client = Client::connect("https://rookery.internal.example", Some(token)).await?;
//! let hosts = client.call("hosts.list", json!({})).await?;
//! let mut log = client.open("jobs.follow", json!({"id": id, "after": 0})).await?;
//! while let Some(bytes) = log.recv().await? { … }
//! ```

pub mod net;

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use rookery_proto::stream::{INITIAL_WINDOW, MAX_FRAME_PAYLOAD, decode_frame, encode_frame};
use rookery_proto::{ErrorBody, ErrorCode, Message, Request, RpcError};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as Ws;

/// What went wrong: the server's error, or the connection's.
#[derive(Debug, Clone)]
pub enum Error {
    /// The server answered with an error.
    Rpc(RpcError),
    /// The connection could not be made, or ended.
    Transport(String),
}

impl Error {
    /// The server's code, or `UNAVAILABLE` for a connection that failed.
    pub fn code(&self) -> ErrorCode {
        match self {
            Error::Rpc(e) => e.error_code(),
            Error::Transport(_) => ErrorCode::Unavailable,
        }
    }

    /// What the server gave besides its message, such as a host's refusal.
    pub fn details(&self) -> Option<&Value> {
        match self {
            Error::Rpc(e) => e.details(),
            Error::Transport(_) => None,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Error::Rpc(e) => e.message.clone(),
            Error::Transport(message) => message.clone(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for Error {}

impl From<RpcError> for Error {
    fn from(e: RpcError) -> Error {
        Error::Rpc(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A notification the server sent outside any call, such as `events.event` or `notify.new`.
#[derive(Debug, Clone)]
pub struct Notice {
    pub method: String,
    pub params: Value,
}

enum Inbound {
    Data(Vec<u8>),
    Eof,
    Closed(Option<ErrorBody>),
}

struct Open {
    inbound: mpsc::UnboundedSender<Inbound>,
    /// Bytes this side may still send.
    credit: Arc<(AtomicI64, Notify)>,
}

#[derive(Default)]
struct Shared {
    calls: Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>,
    streams: Mutex<HashMap<u32, Open>>,
    /// What came for a stream before its call answered with its id.
    early: Mutex<HashMap<u32, Vec<Inbound>>>,
    notices: Mutex<Option<mpsc::UnboundedSender<Notice>>>,
    closed: Mutex<Option<String>>,
    next: AtomicU64,
}

/// A connection to a server on `/ws`. Cheap to clone; the connection ends when the last clone goes.
#[derive(Clone)]
pub struct Client {
    out: mpsc::UnboundedSender<Ws>,
    shared: Arc<Shared>,
    /// The server, as it was named.
    pub server: String,
}

/// How a WebSocket handshake failed, as the server said it.
fn refused(error: tokio_tungstenite::tungstenite::Error) -> Error {
    use tokio_tungstenite::tungstenite;
    match error {
        tungstenite::Error::Http(response) => {
            let status = response.status().as_u16();
            let body = response.body().clone().unwrap_or_default();
            let text = String::from_utf8_lossy(&body);
            let said = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
                .unwrap_or_else(|| text.trim().to_owned());
            let code = match status {
                401 => ErrorCode::Unauthenticated,
                403 => ErrorCode::Forbidden,
                503 => ErrorCode::Unavailable,
                _ => ErrorCode::Internal,
            };
            let message = match (status, said.is_empty()) {
                (401, true) => {
                    "the server took no session or token: sign in with rookery login".into()
                }
                (503, true) => {
                    "the server is a standby: another one leads, or none does yet".into()
                }
                (_, true) => format!("the server answered HTTP {status}"),
                (_, false) => said,
            };
            Error::Rpc(RpcError::new(code, message))
        }
        other => Error::Transport(other.to_string()),
    }
}

impl Client {
    /// Connects to `server`, `https://…`, `http://…` or `unix:/path`, with a token for a server over
    /// the network.
    pub async fn connect(server: &str, token: Option<&str>) -> Result<Client> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let target = net::target(server, "/ws").map_err(Error::Transport)?;
        let stream = net::connect(&target).await.map_err(Error::Transport)?;
        let mut request = format!("ws://{}{}", target.authority, target.path)
            .into_client_request()
            .map_err(|e| Error::Transport(e.to_string()))?;
        if let Some(token) = token.filter(|t| !t.is_empty()) {
            let value = format!("Bearer {token}")
                .parse()
                .map_err(|_| Error::Transport("the token is not one header line".into()))?;
            request.headers_mut().insert("authorization", value);
        }
        let (socket, _) = tokio_tungstenite::client_async(request, stream)
            .await
            .map_err(refused)?;
        Ok(Client::over(socket, server))
    }

    /// Takes over an open WebSocket to `/ws`.
    pub fn over<S>(socket: tokio_tungstenite::WebSocketStream<S>, server: &str) -> Client
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (mut sink, mut source) = socket.split();
        let (out, mut outgoing) = mpsc::unbounded_channel::<Ws>();
        let shared = Arc::new(Shared::default());
        tokio::spawn(async move {
            while let Some(message) = outgoing.recv().await {
                if sink.send(message).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });
        let reader = shared.clone();
        let answer = out.clone();
        tokio::spawn(async move {
            let reason = loop {
                match source.next().await {
                    Some(Ok(Ws::Text(text))) => reader.text(text.as_str()),
                    Some(Ok(Ws::Binary(bytes))) => {
                        if let Some((id, payload)) = decode_frame(&bytes) {
                            reader.deliver(id, Inbound::Data(payload.to_vec()));
                        }
                    }
                    Some(Ok(Ws::Ping(data))) => {
                        let _ = answer.send(Ws::Pong(data));
                    }
                    Some(Ok(Ws::Close(frame))) => {
                        break match frame {
                            Some(frame) if !frame.reason.is_empty() => {
                                format!("the server closed the connection: {}", frame.reason)
                            }
                            _ => "the server closed the connection".to_owned(),
                        };
                    }
                    None => break "the server closed the connection".to_owned(),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => break e.to_string(),
                }
            };
            reader.close(&reason);
        });
        Client {
            out,
            shared,
            server: server.to_owned(),
        }
    }

    /// Whether the connection is still there.
    pub fn usable(&self) -> bool {
        self.shared.closed.lock().unwrap().is_none()
    }

    /// The notifications that come outside calls, from now on. One receiver at a time.
    pub fn notices(&self) -> mpsc::UnboundedReceiver<Notice> {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.shared.notices.lock().unwrap() = Some(tx);
        rx
    }

    fn send(&self, message: Message) -> Result<()> {
        self.out
            .send(Ws::Text(message.to_text().into()))
            .map_err(|_| Error::Transport("the connection closed".into()))
    }

    /// Calls a method and waits for its result.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        if let Some(reason) = self.shared.closed.lock().unwrap().clone() {
            return Err(Error::Transport(reason));
        }
        let id = self.shared.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        self.shared.calls.lock().unwrap().insert(id, tx);
        if let Err(e) = self.send(Message::Request(Request::new(id as i64, method, params))) {
            self.shared.calls.lock().unwrap().remove(&id);
            return Err(e);
        }
        rx.await
            .unwrap_or_else(|_| Err(Error::Transport("the connection closed".into())))
    }

    /// Calls a method that opens a stream, and gives the stream.
    pub async fn connect_agents(&self, env: &str) -> Result<Stream> {
        self.open("agents.connect", json!({ "env": env })).await
    }

    /// Opens a method that returns a binary stream.
    pub async fn open(&self, method: &str, params: Value) -> Result<Stream> {
        let opened = self.call(method, params).await?;
        let id = opened["stream"]
            .as_u64()
            .ok_or_else(|| Error::Transport(format!("{method} opened no stream: {opened}")))?
            as u32;
        let (inbound, incoming) = mpsc::unbounded_channel();
        let credit = Arc::new((AtomicI64::new(INITIAL_WINDOW as i64), Notify::new()));
        {
            let mut streams = self.shared.streams.lock().unwrap();
            let early = self.shared.early.lock().unwrap().remove(&id);
            for item in early.into_iter().flatten() {
                let _ = inbound.send(item);
            }
            streams.insert(
                id,
                Open {
                    inbound,
                    credit: credit.clone(),
                },
            );
        }
        Ok(Stream {
            id,
            offset: opened["offset"].as_u64(),
            client: self.clone(),
            incoming,
            credit,
            ended: false,
            closed: false,
        })
    }

    fn notify(&self, method: &str, params: Value) {
        let _ = self.send(Message::Notification(rookery_proto::Notification::new(
            method, params,
        )));
    }
}

impl Shared {
    fn text(&self, text: &str) {
        match Message::parse(text) {
            Ok(Message::Response(response)) => {
                let Some(rookery_proto::Id::Num(id)) = response.id else {
                    return;
                };
                let id = id as u64;
                if let Some(tx) = self.calls.lock().unwrap().remove(&id) {
                    let _ = tx.send(response.into_result().map_err(Error::Rpc));
                }
            }
            Ok(Message::Notification(n)) => {
                let stream = n.params["stream"].as_u64().map(|id| id as u32);
                match (n.method.as_str(), stream) {
                    ("streams.credit", Some(id)) => {
                        let bytes = n.params["bytes"].as_i64().unwrap_or(0);
                        if let Some(open) = self.streams.lock().unwrap().get(&id) {
                            open.credit.0.fetch_add(bytes, Ordering::AcqRel);
                            open.credit.1.notify_waiters();
                        }
                    }
                    ("streams.eof", Some(id)) => self.deliver(id, Inbound::Eof),
                    ("streams.close", Some(id)) => {
                        let error = serde_json::from_value(n.params["error"].clone()).ok();
                        self.deliver(id, Inbound::Closed(error));
                    }
                    _ => {
                        if let Some(tx) = self.notices.lock().unwrap().as_ref() {
                            let _ = tx.send(Notice {
                                method: n.method,
                                params: n.params,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn deliver(&self, id: u32, item: Inbound) {
        let streams = self.streams.lock().unwrap();
        match streams.get(&id) {
            Some(open) => {
                let _ = open.inbound.send(item);
            }
            // Before its call answered: kept until it is opened.
            None => self.early.lock().unwrap().entry(id).or_default().push(item),
        }
    }

    fn close(&self, reason: &str) {
        *self.closed.lock().unwrap() = Some(reason.to_owned());
        for (_, tx) in self.calls.lock().unwrap().drain() {
            let _ = tx.send(Err(Error::Transport(reason.to_owned())));
        }
        for (_, open) in self.streams.lock().unwrap().drain() {
            let _ = open.inbound.send(Inbound::Closed(Some(ErrorBody {
                code: ErrorCode::Unavailable,
                message: reason.to_owned(),
                details: None,
            })));
            open.credit.1.notify_waiters();
        }
    }
}

/// A stream a method opened. Dropping it closes it.
pub struct Stream {
    id: u32,
    /// Where a source that can be resumed starts.
    pub offset: Option<u64>,
    client: Client,
    incoming: mpsc::UnboundedReceiver<Inbound>,
    credit: Arc<(AtomicI64, Notify)>,
    ended: bool,
    closed: bool,
}

impl Stream {
    pub fn id(&self) -> u32 {
        self.id
    }

    /// The next bytes; `None` once the server sends no more. A stream the server closed with an error
    /// gives it.
    pub async fn recv(&mut self) -> Result<Option<Vec<u8>>> {
        if self.ended {
            return Ok(None);
        }
        match self.incoming.recv().await {
            Some(Inbound::Data(data)) => {
                self.client.notify(
                    "streams.credit",
                    json!({"stream": self.id, "bytes": data.len()}),
                );
                Ok(Some(data))
            }
            Some(Inbound::Eof) => {
                self.ended = true;
                Ok(None)
            }
            Some(Inbound::Closed(error)) => {
                self.ended = true;
                self.closed = true;
                match error {
                    Some(error) => Err(Error::Rpc(RpcError::new(error.code, error.message))),
                    None => Ok(None),
                }
            }
            None => {
                self.ended = true;
                Err(Error::Transport("the connection closed".into()))
            }
        }
    }

    /// Lines of JSON, as `jobs.follow` and the logs send them: the next one, or `None` at the end.
    pub async fn line(&mut self, buffer: &mut Vec<u8>) -> Result<Option<Value>> {
        loop {
            if let Some(end) = buffer.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buffer.drain(..=end).collect();
                let line = &line[..line.len() - 1];
                if line.is_empty() {
                    continue;
                }
                return serde_json::from_slice(line)
                    .map(Some)
                    .map_err(|e| Error::Transport(format!("a line of the stream: {e}")));
            }
            match self.recv().await? {
                Some(data) => buffer.extend_from_slice(&data),
                None if buffer.is_empty() => return Ok(None),
                None => {
                    let rest = std::mem::take(buffer);
                    return serde_json::from_slice(&rest)
                        .map(Some)
                        .map_err(|e| Error::Transport(format!("a line of the stream: {e}")));
                }
            }
        }
    }

    /// Sends bytes, waiting for credit.
    pub async fn send(&self, data: &[u8]) -> Result<()> {
        for chunk in data.chunks(MAX_FRAME_PAYLOAD) {
            loop {
                let waiting = self.credit.1.notified();
                if self.client.shared.closed.lock().unwrap().is_some() {
                    return Err(Error::Transport("the connection closed".into()));
                }
                if self.credit.0.load(Ordering::Acquire) >= chunk.len() as i64 {
                    break;
                }
                waiting.await;
            }
            self.credit
                .0
                .fetch_sub(chunk.len() as i64, Ordering::AcqRel);
            self.client
                .out
                .send(Ws::Binary(encode_frame(self.id, chunk).into()))
                .map_err(|_| Error::Transport("the connection closed".into()))?;
        }
        Ok(())
    }

    /// This side sends no more.
    pub fn eof(&self) {
        self.client
            .notify("streams.eof", json!({"stream": self.id}));
    }

    /// A handle to send on while another task reads.
    pub fn sender(&self) -> Sender {
        Sender {
            id: self.id,
            client: self.client.clone(),
            credit: self.credit.clone(),
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.client.shared.streams.lock().unwrap().remove(&self.id);
        if !self.closed {
            self.client
                .notify("streams.close", json!({"stream": self.id}));
        }
    }
}

/// Sends on a stream that another task reads.
#[derive(Clone)]
pub struct Sender {
    id: u32,
    client: Client,
    credit: Arc<(AtomicI64, Notify)>,
}

impl Sender {
    pub async fn send(&self, data: &[u8]) -> Result<()> {
        for chunk in data.chunks(MAX_FRAME_PAYLOAD) {
            loop {
                let waiting = self.credit.1.notified();
                if self.client.shared.closed.lock().unwrap().is_some() {
                    return Err(Error::Transport("the connection closed".into()));
                }
                if self.credit.0.load(Ordering::Acquire) >= chunk.len() as i64 {
                    break;
                }
                waiting.await;
            }
            self.credit
                .0
                .fetch_sub(chunk.len() as i64, Ordering::AcqRel);
            self.client
                .out
                .send(Ws::Binary(encode_frame(self.id, chunk).into()))
                .map_err(|_| Error::Transport("the connection closed".into()))?;
        }
        Ok(())
    }

    pub fn eof(&self) {
        self.client
            .notify("streams.eof", json!({"stream": self.id}));
    }
}

/// One call over `POST /rpc`, for a script's single call with a token.
pub async fn rpc(server: &str, token: &str, method: &str, params: Value) -> Result<Value> {
    let body = Message::Request(Request::new(1, method, params)).to_text();
    let answer = net::send("POST", server, "/rpc", token, body.into_bytes())
        .await
        .map_err(Error::Transport)?;
    let text = String::from_utf8_lossy(&answer.body);
    match Message::parse(&text) {
        Ok(Message::Response(response)) => response.into_result().map_err(Error::Rpc),
        _ => Err(Error::Transport(format!(
            "the server answered HTTP {}: {}",
            answer.status,
            text.trim()
        ))),
    }
}
