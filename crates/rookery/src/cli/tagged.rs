//! A host's stream as the server carries it for a session: each frame with a byte before it, `t` for
//! the host's JSON and `b` for bytes, both ways.

use super::fail::{Fail, Result};
use serde_json::Value;

/// One frame of a session's stream.
pub enum Piece {
    Text(Value),
    Binary(Vec<u8>),
}

/// A session's stream: its frames as they come, and frames sent in order.
pub struct Tagged {
    stream: rookery_client::Stream,
    queue: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
}

impl Tagged {
    pub fn new(stream: rookery_client::Stream) -> Tagged {
        let sender = stream.sender();
        let (queue, mut queued) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        super::client::runtime().spawn(async move {
            while let Some(frame) = queued.recv().await {
                if sender.send(&frame).await.is_err() {
                    return;
                }
            }
        });
        Tagged { stream, queue }
    }

    /// The next frame; `None` once the stream has ended.
    pub async fn recv(&mut self) -> Option<Result<Piece>> {
        loop {
            match self.stream.recv().await {
                Ok(Some(frame)) => match frame.split_first() {
                    Some((b't', text)) => {
                        return Some(Ok(Piece::Text(
                            serde_json::from_slice(text).unwrap_or(Value::Null),
                        )));
                    }
                    Some((_, bytes)) => return Some(Ok(Piece::Binary(bytes.to_vec()))),
                    None => continue,
                },
                Ok(None) => return None,
                Err(error) => return Some(Err(Fail::from(error))),
            }
        }
    }

    /// Send a frame, after those sent before.
    pub fn push(&self, piece: Piece) -> Result<()> {
        let frame = match piece {
            Piece::Text(value) => [&b"t"[..], value.to_string().as_bytes()].concat(),
            Piece::Binary(bytes) => [&b"b"[..], &bytes[..]].concat(),
        };
        self.queue
            .send(frame)
            .map_err(|_| Fail::unreachable("The connection closed"))
    }
}
