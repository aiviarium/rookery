//! The server's side of the RFB handshake with a host's VNC server: version 3.8, then no security if
//! the server offers it, or VNC authentication with the password the server read on the host. The
//! viewer is then told the server needs none, and the bytes pass unchanged.

use std::time::Duration;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use des::Des;
use des::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rookery_api::client::Frame;
use rookery_core::link::LinkStream;
use rookery_kernel::RpcError;

/// The answer to a VNC authentication challenge: the challenge encrypted by DES with the password
/// as its key, each key byte's bits reversed, as VNC does it.
///
/// The RFB protocol (RFC 6143 §7.2.2) mandates single DES for VNC Authentication, so CodeQL weak-crypto
/// alerts on this cipher use are dismissed.
pub fn response(password: &str, challenge: &[u8; 16]) -> [u8; 16] {
    let mut key = [0u8; 8];
    for (i, b) in password.bytes().take(8).enumerate() {
        key[i] = b.reverse_bits();
    }
    let cipher = Des::new_from_slice(&key).expect("an 8-byte key");
    let mut out = [0u8; 16];
    for (i, chunk) in challenge.chunks(8).enumerate() {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        out[i * 8..i * 8 + 8].copy_from_slice(&block);
    }
    out
}

/// The VNC server, through the host's stream to its port.
pub struct Peer {
    sink: SplitSink<LinkStream, Frame>,
    source: SplitStream<LinkStream>,
    buffer: Vec<u8>,
}

fn u32_of(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

impl Peer {
    pub fn new(stream: LinkStream) -> Peer {
        let (sink, source) = stream.split();
        Peer {
            sink,
            source,
            buffer: Vec::new(),
        }
    }

    async fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, RpcError> {
        while self.buffer.len() < n {
            match tokio::time::timeout(Duration::from_secs(15), self.source.next()).await {
                Ok(Some(Ok(Frame::Binary(bytes)))) => self.buffer.extend_from_slice(&bytes),
                Ok(Some(Ok(Frame::Text(text)))) if text.as_str().contains("\"eof\"") => {
                    return Err(RpcError::unavailable(
                        "the VNC server closed the connection",
                    ));
                }
                Ok(Some(Ok(_))) => {}
                Ok(_) => {
                    return Err(RpcError::unavailable(
                        "the VNC server closed the connection",
                    ));
                }
                Err(_) => return Err(RpcError::timeout("the VNC server did not answer")),
            }
        }
        Ok(self.buffer.drain(..n).collect())
    }

    async fn send(&mut self, bytes: &[u8]) -> Result<(), RpcError> {
        self.sink
            .send(Frame::Binary(bytes.to_vec().into()))
            .await
            .map_err(|_| RpcError::unavailable("the VNC connection closed"))
    }

    pub async fn handshake(&mut self, password: Option<&str>) -> Result<(), RpcError> {
        let version = self.read_exact(12).await?;
        if !version.starts_with(b"RFB ") {
            return Err(RpcError::unavailable("that port does not speak VNC"));
        }
        self.send(b"RFB 003.008\n").await?;
        let count = self.read_exact(1).await?[0] as usize;
        if count == 0 {
            let len = u32_of(&self.read_exact(4).await?) as usize;
            let reason = self.read_exact(len.min(4096)).await?;
            return Err(RpcError::unavailable(format!(
                "the VNC server refused: {}",
                String::from_utf8_lossy(&reason)
            )));
        }
        let types = self.read_exact(count).await?;
        if types.contains(&1) {
            self.send(&[1]).await?;
        } else if types.contains(&2) {
            let password = password.ok_or_else(|| {
                RpcError::unavailable(
                    "the VNC server needs a password, and the host has none for it",
                )
            })?;
            self.send(&[2]).await?;
            let challenge: [u8; 16] = self.read_exact(16).await?.try_into().expect("16 bytes");
            self.send(&response(password, &challenge)).await?;
        } else {
            return Err(RpcError::unavailable(format!(
                "the VNC server offers no security type the server speaks: {types:?}"
            )));
        }
        if u32_of(&self.read_exact(4).await?) != 0 {
            return Err(RpcError::unavailable("the VNC server refused the password"));
        }
        Ok(())
    }

    /// What was read past the handshake, and the two halves of the stream.
    pub fn into_parts(
        self,
    ) -> (
        Vec<u8>,
        SplitSink<LinkStream, Frame>,
        SplitStream<LinkStream>,
    ) {
        (self.buffer, self.sink, self.source)
    }
}

/// noVNC, through its WebSocket.
#[derive(Default)]
pub struct Viewer {
    buffer: Vec<u8>,
}

impl Viewer {
    pub async fn read_exact(
        &mut self,
        source: &mut SplitStream<WebSocket>,
        n: usize,
    ) -> Result<Vec<u8>, RpcError> {
        while self.buffer.len() < n {
            match tokio::time::timeout(Duration::from_secs(30), source.next()).await {
                Ok(Some(Ok(WsMessage::Binary(bytes)))) => self.buffer.extend_from_slice(&bytes),
                Ok(Some(Ok(WsMessage::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => {
                    return Err(RpcError::unavailable("the viewer went away"));
                }
                Ok(Some(Ok(_))) => {}
                Err(_) => return Err(RpcError::timeout("the viewer did not answer")),
            }
        }
        Ok(self.buffer.drain(..n).collect())
    }

    pub fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_is_des_with_the_key_bits_reversed() {
        // The same block twice encrypts alike, and a password changes it.
        let r = response("password", &[0u8; 16]);
        assert_eq!(&r[..8], &r[8..]);
        assert_ne!(r, [0u8; 16]);
        assert_ne!(response("other", &[0u8; 16]), r);
        // A known answer: DES of a zero block under a zero key is 8CA64DE9C1B123A7.
        let zero = response("\0\0\0\0\0\0\0\0", &[0u8; 16]);
        assert_eq!(hex::encode(&zero[..8]), "8ca64de9c1b123a7");
    }
}
