//! Stream channels: binary frames for data, JSON-RPC notifications for control.
//!
//! A frame is 4 bytes of stream id, big endian, then the payload. A method opens a stream and returns its
//! id. The server gives out the ids: even for streams a client asked for, odd for those the server opens.
//! Flow control is credit in bytes for each direction, starting at [`INITIAL_WINDOW`]; the reader grants
//! more with `streams.credit` as it takes data. A peer that sends more than its credit loses the stream.

use serde::{Deserialize, Serialize};

use crate::error::ErrorBody;

/// Bytes a sender may send on a new stream before it is given credit.
pub const INITIAL_WINDOW: u32 = 256 * 1024;

/// The largest payload of one frame.
pub const MAX_FRAME_PAYLOAD: usize = 64 * 1024;

/// One data frame.
pub fn encode_frame(stream: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&stream.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// The stream id and payload of a frame, or `None` when it is shorter than its id.
pub fn decode_frame(frame: &[u8]) -> Option<(u32, &[u8])> {
    let id = u32::from_be_bytes(frame.get(..4)?.try_into().ok()?);
    Some((id, &frame[4..]))
}

/// `streams.credit`: the receiver took `bytes`, and the sender may send that much more.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StreamCredit {
    pub stream: u32,
    pub bytes: u32,
}

/// `streams.close`: ends both directions. An error says why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StreamClose {
    pub stream: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub error: Option<ErrorBody>,
}

/// `streams.eof`: the sender sends no more data. The other direction stays open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StreamEof {
    pub stream: u32,
}

/// The result of a method that opens a stream. A source that can be resumed says where it starts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StreamOpened {
    pub stream: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub offset: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames() {
        let frame = encode_frame(258, b"hi");
        assert_eq!(frame, [0, 0, 1, 2, b'h', b'i']);
        assert_eq!(decode_frame(&frame), Some((258, &b"hi"[..])));
        assert_eq!(decode_frame(&[0, 1]), None);
    }
}
