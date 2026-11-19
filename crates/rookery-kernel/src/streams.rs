//! Stream channels with credit.
//!
//! Each connection has its own [`Streams`]. A method opens a stream and returns its id; data flows in
//! binary frames, control in `streams.credit`, `streams.eof` and `streams.close`. Each direction starts
//! with [`INITIAL_WINDOW`] bytes of credit. A writer waits when it has none left, a reader grants it back
//! as it takes data, and a peer that sends more than its credit loses the stream.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use rookery_proto::stream::{
    INITIAL_WINDOW, MAX_FRAME_PAYLOAD, StreamClose, StreamCredit, StreamEof, StreamOpened,
    decode_frame, encode_frame,
};
use rookery_proto::{Empty, ErrorBody, ErrorCode, RpcError};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

use crate::conn::{OutTx, send_notification};
use crate::methods::{Call, Level, Methods};

const MAX_STREAMS: usize = 32;
const MAX_QUEUED_DATA_FRAMES: usize = 256;

pub fn register(methods: &mut Methods) {
    methods
        .add("streams.credit", |call: Call, p: StreamCredit| async move {
            call.conn()?.streams().credit(p.stream, p.bytes);
            Ok(Empty {})
        })
        .summary("The caller took bytes of a stream, and the server may send that much more.")
        .level(Level::Own);
    methods
        .add("streams.eof", |call: Call, p: StreamEof| async move {
            call.conn()?.streams().remote_eof(p.stream);
            Ok(Empty {})
        })
        .summary("The caller sends no more data on a stream.")
        .level(Level::Own);
    methods
        .add("streams.close", |call: Call, p: StreamClose| async move {
            call.conn()?.streams().remote_close(p.stream, p.error);
            Ok(Empty {})
        })
        .summary("Ends a stream in both directions.")
        .level(Level::Own);
    methods
        .add("streams.echo", |call: Call, _: Empty| async move {
            let stream = call.conn()?.streams().open(Initiator::Peer)?;
            let id = stream.id();
            tokio::spawn(echo(stream));
            Ok(StreamOpened {
                stream: id,
                offset: None,
            })
        })
        .summary("A stream that sends back every byte, to check a connection end to end.")
        .stream()
        .level(Level::Own);
}

async fn echo(mut stream: Stream) {
    while let Some(item) = stream.read().await {
        match item {
            Inbound::Data(data) => {
                if stream.write(&data).await.is_err() {
                    return;
                }
            }
            Inbound::Eof => {
                stream.eof();
                stream.close(None);
                return;
            }
            Inbound::Closed(_) => return,
        }
    }
}

/// What a stream's reader receives.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Data(Vec<u8>),
    /// The peer sends no more data.
    Eof,
    /// The peer closed the stream, or the socket went away.
    Closed(Option<ErrorBody>),
}

/// Who asked for a stream: the peer's get even ids, the server's odd ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Initiator {
    Peer,
    Server,
}

struct Shared {
    id: u32,
    /// Bytes the server may still send.
    send_credit: Semaphore,
    /// Bytes the peer may still send.
    recv_window: AtomicI64,
    inbound: mpsc::Sender<Inbound>,
    peer_eof: AtomicBool,
    credit_update: Mutex<()>,
    closed: AtomicBool,
    closed_signal: CancellationToken,
}

impl Shared {
    fn mark_closed(&self) {
        self.closed.store(true, Ordering::Release);
        self.send_credit.close();
        self.closed_signal.cancel();
    }
}

struct Inner {
    out: OutTx,
    next_even: AtomicU32,
    next_odd: AtomicU32,
    map: Mutex<HashMap<u32, Arc<Shared>>>,
}

/// The streams of one socket.
#[derive(Clone)]
pub struct Streams(Arc<Inner>);

impl Streams {
    pub fn new(out: OutTx) -> Streams {
        Streams(Arc::new(Inner {
            out,
            next_even: AtomicU32::new(2),
            next_odd: AtomicU32::new(1),
            map: Mutex::new(HashMap::new()),
        }))
    }

    pub fn open(&self, by: Initiator) -> Result<Stream, RpcError> {
        let mut map = self.0.map.lock().unwrap();
        if map.len() >= MAX_STREAMS {
            return Err(RpcError::unavailable("too many open streams"));
        }
        let id = match by {
            Initiator::Peer => self.0.next_even.fetch_add(2, Ordering::Relaxed),
            Initiator::Server => self.0.next_odd.fetch_add(2, Ordering::Relaxed),
        };
        let (tx, rx) = mpsc::channel(MAX_QUEUED_DATA_FRAMES + 2);
        let shared = Arc::new(Shared {
            id,
            send_credit: Semaphore::new(INITIAL_WINDOW as usize),
            recv_window: AtomicI64::new(INITIAL_WINDOW as i64),
            inbound: tx,
            peer_eof: AtomicBool::new(false),
            credit_update: Mutex::new(()),
            closed: AtomicBool::new(false),
            closed_signal: CancellationToken::new(),
        });
        map.insert(id, shared.clone());
        Ok(Stream {
            reader: StreamReader {
                shared: shared.clone(),
                rx,
                streams: self.clone(),
            },
            writer: StreamWriter {
                shared,
                streams: self.clone(),
            },
            guard: Arc::new(CloseGuard {
                id,
                streams: self.clone(),
            }),
        })
    }

    fn get(&self, id: u32) -> Option<Arc<Shared>> {
        self.0.map.lock().unwrap().get(&id).cloned()
    }

    fn remove(&self, id: u32) -> Option<Arc<Shared>> {
        self.0.map.lock().unwrap().remove(&id)
    }

    pub fn len(&self) -> usize {
        self.0.map.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A binary frame from the peer.
    pub fn deliver(&self, frame: &[u8]) {
        let Some((id, payload)) = decode_frame(frame) else {
            return;
        };
        let Some(shared) = self.get(id) else { return };
        if payload.is_empty() || shared.peer_eof.load(Ordering::Acquire) {
            return;
        }
        let left = shared
            .recv_window
            .fetch_sub(payload.len() as i64, Ordering::AcqRel)
            - payload.len() as i64;
        // Keep room for EOF and close even when the data queue is full.
        if left < 0 || payload.len() > MAX_FRAME_PAYLOAD || shared.inbound.capacity() <= 2 {
            let error = ErrorBody {
                code: ErrorCode::Invalid,
                message: format!("stream {id}: the peer exceeded the receive limit"),
                details: None,
            };
            self.close_local(id, Some(error));
            return;
        }
        let _ = shared.inbound.try_send(Inbound::Data(payload.to_vec()));
    }

    /// `streams.credit` from the peer.
    pub fn credit(&self, id: u32, bytes: u32) {
        if let Some(shared) = self.get(id) {
            let _update = shared.credit_update.lock().unwrap();
            let room = INITIAL_WINDOW as usize - shared.send_credit.available_permits();
            shared.send_credit.add_permits(room.min(bytes as usize));
        }
    }

    pub fn remote_eof(&self, id: u32) {
        if let Some(shared) = self.get(id)
            && !shared.peer_eof.swap(true, Ordering::AcqRel)
        {
            let _ = shared.inbound.try_send(Inbound::Eof);
        }
    }

    pub fn remote_close(&self, id: u32, error: Option<ErrorBody>) {
        if let Some(shared) = self.remove(id) {
            shared.mark_closed();
            let _ = shared.inbound.try_send(Inbound::Closed(error));
        }
    }

    /// Closes from the server's side and tells the peer.
    fn close_local(&self, id: u32, error: Option<ErrorBody>) {
        if let Some(shared) = self.remove(id) {
            shared.mark_closed();
            let _ = shared.inbound.try_send(Inbound::Closed(error.clone()));
            send_notification(
                &self.0.out,
                "streams.close",
                StreamClose { stream: id, error },
            );
        }
    }

    /// The socket went away.
    pub fn close_all(&self) {
        let all: Vec<_> = self.0.map.lock().unwrap().drain().collect();
        for (_, shared) in all {
            shared.mark_closed();
            let _ = shared.inbound.try_send(Inbound::Closed(None));
        }
    }
}

/// Closes the stream when the last half of it is dropped.
struct CloseGuard {
    id: u32,
    streams: Streams,
}

impl Drop for CloseGuard {
    fn drop(&mut self) {
        self.streams.close_local(self.id, None);
    }
}

/// The server's end of a stream. Dropping it closes the stream.
pub struct Stream {
    reader: StreamReader,
    writer: StreamWriter,
    guard: Arc<CloseGuard>,
}

struct StreamReader {
    shared: Arc<Shared>,
    rx: mpsc::Receiver<Inbound>,
    streams: Streams,
}

#[derive(Clone)]
struct StreamWriter {
    shared: Arc<Shared>,
    streams: Streams,
}

/// The reading half of a [`Stream`] after [`Stream::split`].
pub struct OwnedReader {
    reader: StreamReader,
    _guard: Arc<CloseGuard>,
}

/// The writing half of a [`Stream`] after [`Stream::split`]. The stream closes when both halves are
/// dropped, or on [`OwnedWriter::close`].
#[derive(Clone)]
pub struct OwnedWriter {
    writer: StreamWriter,
    _guard: Arc<CloseGuard>,
}

impl Stream {
    pub fn id(&self) -> u32 {
        self.writer.shared.id
    }

    /// Sends data, waiting for credit. Data larger than a frame goes in several.
    pub async fn write(&self, data: &[u8]) -> Result<(), RpcError> {
        self.writer.write(data).await
    }

    /// The next item from the peer. Data is credited back to the peer as it is taken.
    pub async fn read(&mut self) -> Option<Inbound> {
        self.reader.read().await
    }

    /// The server sends no more data.
    pub fn eof(&self) {
        self.writer.eof()
    }

    pub fn close(&self, error: Option<ErrorBody>) {
        self.writer.close(error)
    }

    pub fn available_credit(&self) -> usize {
        self.writer.shared.send_credit.available_permits()
    }

    /// Splits into halves that can live in different tasks.
    pub fn split(self) -> (OwnedReader, OwnedWriter) {
        let Stream {
            reader,
            writer,
            guard,
        } = self;
        (
            OwnedReader {
                reader,
                _guard: guard.clone(),
            },
            OwnedWriter {
                writer,
                _guard: guard,
            },
        )
    }
}

impl StreamReader {
    async fn read(&mut self) -> Option<Inbound> {
        let item = self.rx.recv().await?;
        if let Inbound::Data(data) = &item
            && !data.is_empty()
            && !self.shared.closed.load(Ordering::Acquire)
        {
            self.shared
                .recv_window
                .fetch_add(data.len() as i64, Ordering::AcqRel);
            let credit = StreamCredit {
                stream: self.shared.id,
                bytes: data.len() as u32,
            };
            send_notification(&self.streams.0.out, "streams.credit", credit);
        }
        Some(item)
    }
}

impl StreamWriter {
    async fn write(&self, data: &[u8]) -> Result<(), RpcError> {
        for chunk in data.chunks(MAX_FRAME_PAYLOAD) {
            let permit = self
                .shared
                .send_credit
                .acquire_many(chunk.len() as u32)
                .await
                .map_err(|_| {
                    RpcError::unavailable(format!("stream {} is closed", self.shared.id))
                })?;
            permit.forget();
            tokio::select! {
                biased;
                _ = self.shared.closed_signal.cancelled() => {
                    return Err(RpcError::unavailable("the stream is closed"));
                }
                result = self.streams.0.out.send_binary(encode_frame(self.shared.id, chunk)) => result?,
            }
        }
        Ok(())
    }

    fn eof(&self) {
        if !self.shared.closed.load(Ordering::Acquire) {
            send_notification(
                &self.streams.0.out,
                "streams.eof",
                StreamEof {
                    stream: self.shared.id,
                },
            );
        }
    }

    fn close(&self, error: Option<ErrorBody>) {
        self.streams.close_local(self.shared.id, error);
    }
}

impl OwnedReader {
    pub fn id(&self) -> u32 {
        self.reader.shared.id
    }
    pub async fn read(&mut self) -> Option<Inbound> {
        self.reader.read().await
    }
}

impl OwnedWriter {
    pub fn id(&self) -> u32 {
        self.writer.shared.id
    }
    pub async fn write(&self, data: &[u8]) -> Result<(), RpcError> {
        self.writer.write(data).await
    }
    pub fn eof(&self) {
        self.writer.eof()
    }
    pub fn close(&self, error: Option<ErrorBody>) {
        self.writer.close(error)
    }
    pub fn is_closed(&self) -> bool {
        self.writer.shared.closed.load(Ordering::Acquire)
    }
    /// Waits until the stream is closed by either side.
    pub async fn closed(&self) {
        self.writer.shared.closed_signal.cancelled().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::Outbound;

    fn text(out: Outbound) -> serde_json::Value {
        match out {
            Outbound::Text(text) => serde_json::from_str(&text).unwrap(),
            Outbound::Binary(_) => panic!("a text frame was expected"),
        }
    }

    #[tokio::test]
    async fn a_writer_waits_for_credit() {
        let (tx, mut rx) = OutTx::channel();
        let streams = Streams::new(tx);
        let stream = Arc::new(streams.open(Initiator::Peer).unwrap());
        assert_eq!(stream.id(), 2);
        let data = vec![7u8; INITIAL_WINDOW as usize + 1000];
        let writing = {
            let stream = stream.clone();
            tokio::spawn(async move { stream.write(&data).await })
        };
        let mut got = 0;
        while got < INITIAL_WINDOW as usize {
            match rx.recv().await.unwrap() {
                Outbound::Binary(frame) => got += frame.len() - 4,
                Outbound::Text(text) => panic!("unexpected {text}"),
            }
        }
        assert_eq!(got, INITIAL_WINDOW as usize);
        // Every permit is used and nothing more is queued: the writer waits.
        tokio::task::yield_now().await;
        assert_eq!(stream.available_credit(), 0);
        assert!(rx.try_recv().is_err() && !writing.is_finished());
        streams.credit(2, 1000);
        let Outbound::Binary(frame) = rx.recv().await.unwrap() else {
            panic!("a frame")
        };
        assert_eq!(frame.len() - 4, 1000);
        writing.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_reader_grants_credit_and_an_overrun_closes() {
        let (tx, mut rx) = OutTx::channel();
        let streams = Streams::new(tx);
        let mut stream = streams.open(Initiator::Server).unwrap();
        assert_eq!(stream.id(), 1);
        streams.deliver(&encode_frame(1, b"hello"));
        assert_eq!(stream.read().await, Some(Inbound::Data(b"hello".to_vec())));
        let credit = text(rx.recv().await.unwrap());
        assert_eq!(credit["method"], "streams.credit");
        assert_eq!(
            credit["params"],
            serde_json::json!({"stream": 1, "bytes": 5})
        );

        streams.deliver(&encode_frame(1, &vec![0u8; INITIAL_WINDOW as usize + 1]));
        let close = text(rx.recv().await.unwrap());
        assert_eq!(close["method"], "streams.close");
        assert_eq!(close["params"]["error"]["code"], "INVALID");
        assert!(matches!(
            stream.read().await,
            Some(Inbound::Closed(Some(_)))
        ));
        assert!(streams.is_empty());
        assert!(stream.write(b"x").await.is_err());
    }

    #[tokio::test]
    async fn empty_data_and_repeated_eof_do_not_fill_the_queue() {
        let (tx, mut rx) = OutTx::channel();
        let streams = Streams::new(tx);
        let mut stream = streams.open(Initiator::Peer).unwrap();
        streams.deliver(&encode_frame(stream.id(), b""));
        streams.deliver(&encode_frame(stream.id(), b"hello"));
        assert_eq!(stream.read().await, Some(Inbound::Data(b"hello".to_vec())));
        let _ = rx.recv().await.unwrap();
        streams.remote_eof(stream.id());
        streams.remote_eof(stream.id());
        streams.remote_close(stream.id(), None);
        assert_eq!(stream.read().await, Some(Inbound::Eof));
        assert_eq!(stream.read().await, Some(Inbound::Closed(None)));
    }

    #[tokio::test]
    async fn credit_never_exceeds_the_window() {
        let (tx, _rx) = OutTx::channel();
        let streams = Streams::new(tx);
        let stream = streams.open(Initiator::Peer).unwrap();
        streams.credit(stream.id(), 1);
        assert!(stream.available_credit() <= INITIAL_WINDOW as usize);
    }

    #[tokio::test]
    async fn eof_close_and_drop() {
        let (tx, mut rx) = OutTx::channel();
        let streams = Streams::new(tx);
        let mut stream = streams.open(Initiator::Peer).unwrap();
        streams.remote_eof(stream.id());
        assert_eq!(stream.read().await, Some(Inbound::Eof));
        streams.remote_close(stream.id(), None);
        assert_eq!(stream.read().await, Some(Inbound::Closed(None)));
        assert!(streams.is_empty());

        // Dropping both halves closes the stream and tells the peer.
        let (reader, writer) = streams.open(Initiator::Peer).unwrap().split();
        drop(reader);
        drop(writer);
        let close = text(rx.recv().await.unwrap());
        assert_eq!(
            (close["method"].as_str(), close["params"]["stream"].as_u64()),
            (Some("streams.close"), Some(4))
        );

        // A writer kept elsewhere sees the peer close it.
        let (_reader, writer) = streams.open(Initiator::Peer).unwrap().split();
        let id = writer.id();
        let waiting = writer.clone();
        let closed = tokio::spawn(async move { waiting.closed().await });
        streams.remote_close(id, None);
        closed.await.unwrap();
        assert!(writer.is_closed());
    }
}
