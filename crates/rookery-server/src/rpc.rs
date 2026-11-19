//! `POST /rpc`: one JSON-RPC call over HTTP, for scripts. It takes an access token, or a device
//! session's token, in `Authorization: Bearer`; cookies are not read, so a page elsewhere cannot call it
//! with a browser's session.

use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rookery_kernel::{Call, Kernel, RpcError};
use rookery_proto::{Message, Response as RpcResponse};

use crate::edge::{Via, caller_of};

fn json(status: StatusCode, response: &RpcResponse) -> Response {
    let body = Message::Response(response.clone()).to_text();
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

pub async fn handle(
    State(kernel): State<Kernel>,
    Extension(via): Extension<Via>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let text = String::from_utf8_lossy(&body);
    let message = match Message::parse(&text) {
        Ok(message) => message,
        Err(refused) => return json(StatusCode::BAD_REQUEST, &refused),
    };
    let (id, method, params) = match message {
        Message::Request(r) => (Some(r.id), r.method, r.params),
        Message::Notification(n) => (None, n.method, n.params),
        Message::Response(_) => {
            return json(
                StatusCode::BAD_REQUEST,
                &RpcResponse::err(None, RpcError::invalid_request("/rpc takes a call")),
            );
        }
    };
    let Some(caller) = caller_of(&kernel, via, &headers, false).await else {
        let refused =
            RpcError::forbidden("sign in first: Authorization: Bearer and an access token");
        return json(StatusCode::UNAUTHORIZED, &RpcResponse::err(id, refused));
    };
    let call = Call::new(kernel.clone(), caller, None);
    let result = kernel.methods.dispatch(call, &method, params).await;
    match id {
        Some(id) => json(StatusCode::OK, &RpcResponse::from_result(id, result)),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}
