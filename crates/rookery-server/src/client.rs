//! A client of the Unix socket, for the CLI on the server's own machine: one JSON-RPC call over HTTP.

use std::path::Path;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use rookery_proto::{Message, Request, RpcError};
use serde_json::Value;

/// Calls `method` on the server at `socket`. A server that is not there, or is a standby, is an error that
/// says so.
pub async fn call(socket: &Path, method: &str, params: Value) -> Result<Value, String> {
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .map_err(|e| format!("no rookery serve answers on {}: {e}", socket.display()))?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(conn);
    let body = Message::Request(Request::new(1, method, params)).to_text();
    let request = hyper::Request::post("/rpc")
        .header(hyper::header::HOST, "localhost")
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .map_err(|e| e.to_string())?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .map_err(|e| e.to_string())?
        .to_bytes();
    let text = String::from_utf8_lossy(&bytes);
    if status == hyper::StatusCode::SERVICE_UNAVAILABLE {
        return Err(
            "this rookery serve is a standby: another one leads, and the command goes to that one"
                .into(),
        );
    }
    match Message::parse(&text) {
        Ok(Message::Response(response)) => {
            response.into_result().map_err(|e: RpcError| e.to_string())
        }
        _ => Err(format!("rookery serve answered {status}: {text}")),
    }
}
