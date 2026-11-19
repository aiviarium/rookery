//! The JSON-RPC 2.0 envelope of `/ws`, `/rpc` and the Unix socket.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::error::RpcError;

pub const VERSION: &str = "2.0";

/// A request id. Rookery's own clients use numbers; strings are accepted from others.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(untagged)]
pub enum Id {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    Num(i64),
    Str(String),
}

impl From<i64> for Id {
    fn from(n: i64) -> Self {
        Id::Num(n)
    }
}

/// A call that expects a response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Request {
    #[cfg_attr(feature = "ts", ts(type = "\"2.0\""))]
    pub jsonrpc: String,
    pub id: Id,
    pub method: String,
    /// Always one object, `{}` when there is nothing to pass.
    #[serde(default = "empty_object")]
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
    pub params: Value,
}

/// A message without an id. The server's own are named like methods: `streams.credit`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Notification {
    #[cfg_attr(feature = "ts", ts(type = "\"2.0\""))]
    pub jsonrpc: String,
    pub method: String,
    #[serde(default = "empty_object")]
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
    pub params: Value,
}

/// The answer to a request: exactly one of `result` and `error` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Response {
    #[cfg_attr(feature = "ts", ts(type = "\"2.0\""))]
    pub jsonrpc: String,
    /// `null` only when the request could not be read far enough to find its id.
    pub id: Option<Id>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub error: Option<RpcError>,
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

/// `"result": null` stays `Some(Null)` instead of becoming `None`.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

impl Request {
    pub fn new(id: impl Into<Id>, method: impl Into<String>, params: Value) -> Self {
        Request {
            jsonrpc: VERSION.into(),
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

impl Notification {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Notification {
            jsonrpc: VERSION.into(),
            method: method.into(),
            params,
        }
    }
}

impl Response {
    pub fn ok(id: Id, result: Value) -> Self {
        Response {
            jsonrpc: VERSION.into(),
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Option<Id>, error: RpcError) -> Self {
        Response {
            jsonrpc: VERSION.into(),
            id,
            result: None,
            error: Some(error),
        }
    }

    pub fn from_result(id: Id, result: Result<Value, RpcError>) -> Self {
        match result {
            Ok(value) => Response::ok(id, value),
            Err(error) => Response::err(Some(id), error),
        }
    }

    pub fn into_result(self) -> Result<Value, RpcError> {
        match (self.result, self.error) {
            (_, Some(error)) => Err(error),
            (Some(value), None) => Ok(value),
            (None, None) => Ok(Value::Null),
        }
    }
}

/// Any message read from a socket.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Request(Request),
    Notification(Notification),
    Response(Response),
}

impl Message {
    /// Reads one text frame. A bad one comes back as the response to send, with its id when it had one.
    #[allow(clippy::result_large_err)]
    pub fn parse(text: &str) -> Result<Message, Response> {
        let value: Value = serde_json::from_str(text)
            .map_err(|e| Response::err(None, RpcError::parse_error(e.to_string())))?;
        Self::from_value(value)
    }

    #[allow(clippy::result_large_err)]
    pub fn from_value(mut value: Value) -> Result<Message, Response> {
        let Some(object) = value.as_object() else {
            return Err(Response::err(
                None,
                RpcError::invalid_request("a message is one object"),
            ));
        };
        let id = object
            .get("id")
            .and_then(|id| serde_json::from_value::<Id>(id.clone()).ok());
        if object.get("jsonrpc").and_then(Value::as_str) != Some(VERSION) {
            return Err(Response::err(
                id,
                RpcError::invalid_request("jsonrpc must be \"2.0\""),
            ));
        }
        if let Some(params) = object.get("params")
            && !(params.is_object() || params.is_null())
        {
            return Err(Response::err(
                id,
                RpcError::invalid("params are one object"),
            ));
        }
        let is_call = object.contains_key("method");
        let has_id = object.get("id").is_some_and(|id| !id.is_null());
        let bad =
            |e: serde_json::Error, id| Response::err(id, RpcError::invalid_request(e.to_string()));
        if !is_call {
            return serde_json::from_value(value)
                .map(Message::Response)
                .map_err(|e| bad(e, id));
        }
        if value.get("params").is_some_and(Value::is_null) {
            value["params"] = empty_object();
        }
        if has_id {
            serde_json::from_value(value)
                .map(Message::Request)
                .map_err(|e| bad(e, id))
        } else {
            serde_json::from_value(value)
                .map(Message::Notification)
                .map_err(|e| bad(e, None))
        }
    }

    pub fn to_text(&self) -> String {
        match self {
            Message::Request(r) => serde_json::to_string(r),
            Message::Notification(n) => serde_json::to_string(n),
            Message::Response(r) => serde_json::to_string(r),
        }
        .expect("wire types serialize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;
    use serde_json::json;

    #[test]
    fn requests_notifications_and_responses() {
        let Message::Request(r) =
            Message::parse(r#"{"jsonrpc":"2.0","id":7,"method":"auth.me"}"#).unwrap()
        else {
            panic!("a request")
        };
        assert_eq!(
            (r.id, r.method.as_str(), r.params),
            (Id::Num(7), "auth.me", json!({}))
        );
        let parsed =
            Message::parse(r#"{"jsonrpc":"2.0","method":"streams.credit","params":null}"#).unwrap();
        assert!(matches!(parsed, Message::Notification(n) if n.params == json!({})));
        let parsed = Message::parse(r#"{"jsonrpc":"2.0","id":"a","result":null}"#).unwrap();
        assert!(matches!(parsed, Message::Response(r) if r.result == Some(Value::Null)));
    }

    #[test]
    fn bad_frames_answer_with_their_id() {
        let refused =
            Message::parse(r#"{"jsonrpc":"2.0","id":3,"method":"x","params":[1]}"#).unwrap_err();
        assert_eq!(refused.id, Some(Id::Num(3)));
        assert_eq!(refused.error.unwrap().error_code(), ErrorCode::Invalid);
        let refused = Message::parse("not json").unwrap_err();
        assert_eq!(
            (refused.id, refused.error.unwrap().code),
            (None, crate::error::PARSE_ERROR)
        );
        let refused = Message::parse(r#"{"jsonrpc":"1.0","id":1,"method":"x"}"#).unwrap_err();
        assert_eq!(refused.id, Some(Id::Num(1)));
    }
}
