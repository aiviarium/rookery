//! The fixed set of errors. `error.code` is the JSON-RPC number and `error.data.code` the name, which
//! survives every hop so that a client can show the right thing.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// The params fail the schema, or the request is wrong.
    Invalid,
    /// The caller lacks the level on the resource.
    Forbidden,
    /// No such method, row or thing.
    NotFound,
    /// A revision or compare-and-swap did not match, or a lease is held.
    Conflict,
    /// Something it needs is not there: a key, a host, or this server is a standby.
    Unavailable,
    /// The call's deadline passed.
    Timeout,
    /// A bug.
    Internal,
    /// The request has no session or token that works: sign in again.
    Unauthenticated,
}

/// JSON-RPC's own numbers for problems with the envelope.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;

impl ErrorCode {
    pub const ALL: [ErrorCode; 8] = [
        ErrorCode::Invalid,
        ErrorCode::Forbidden,
        ErrorCode::NotFound,
        ErrorCode::Conflict,
        ErrorCode::Unavailable,
        ErrorCode::Timeout,
        ErrorCode::Internal,
        ErrorCode::Unauthenticated,
    ];

    /// The JSON-RPC number of the code.
    pub fn number(self) -> i64 {
        match self {
            ErrorCode::Invalid => -32602,
            ErrorCode::Forbidden => -32001,
            ErrorCode::NotFound => -32002,
            ErrorCode::Conflict => -32003,
            ErrorCode::Unavailable => -32004,
            ErrorCode::Timeout => -32005,
            ErrorCode::Internal => -32603,
            ErrorCode::Unauthenticated => -32000,
        }
    }

    /// The code of a JSON-RPC number, the standard ones of the envelope included.
    pub fn from_number(n: i64) -> ErrorCode {
        match n {
            -32602 | PARSE_ERROR | INVALID_REQUEST => ErrorCode::Invalid,
            -32001 => ErrorCode::Forbidden,
            -32002 | METHOD_NOT_FOUND => ErrorCode::NotFound,
            -32003 => ErrorCode::Conflict,
            -32004 => ErrorCode::Unavailable,
            -32005 => ErrorCode::Timeout,
            -32000 => ErrorCode::Unauthenticated,
            _ => ErrorCode::Internal,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Invalid => "INVALID",
            ErrorCode::Forbidden => "FORBIDDEN",
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::Conflict => "CONFLICT",
            ErrorCode::Unavailable => "UNAVAILABLE",
            ErrorCode::Timeout => "TIMEOUT",
            ErrorCode::Internal => "INTERNAL",
            ErrorCode::Unauthenticated => "UNAUTHENTICATED",
        }
    }

    /// The HTTP status `/rpc` answers a call that failed with this code.
    pub fn http_status(self) -> u16 {
        match self {
            ErrorCode::Invalid => 400,
            ErrorCode::Forbidden => 403,
            ErrorCode::NotFound => 404,
            ErrorCode::Conflict => 409,
            ErrorCode::Unavailable => 503,
            ErrorCode::Timeout => 504,
            ErrorCode::Internal => 500,
            ErrorCode::Unauthenticated => 401,
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `error.data` of a JSON-RPC error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ErrorData {
    pub code: ErrorCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
    pub details: Option<Value>,
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub data: Option<ErrorData>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.error_code(), self.message)
    }
}

impl std::error::Error for RpcError {}

impl RpcError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        RpcError {
            code: code.number(),
            message: message.into(),
            data: Some(ErrorData {
                code,
                details: None,
            }),
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        let code = self.error_code();
        self.data = Some(ErrorData {
            code,
            details: Some(details),
        });
        self
    }

    /// The name of the code: `data.code` when there is one, else read from the number.
    pub fn error_code(&self) -> ErrorCode {
        self.data
            .as_ref()
            .map(|d| d.code)
            .unwrap_or_else(|| ErrorCode::from_number(self.code))
    }

    pub fn details(&self) -> Option<&Value> {
        self.data.as_ref().and_then(|d| d.details.as_ref())
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Invalid, message)
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Forbidden, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unavailable, message)
    }
    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Timeout, message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }
    pub fn unauthenticated(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthenticated, message)
    }

    fn standard(number: i64, code: ErrorCode, message: String) -> Self {
        RpcError {
            code: number,
            message,
            data: Some(ErrorData {
                code,
                details: None,
            }),
        }
    }
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::standard(PARSE_ERROR, ErrorCode::Invalid, message.into())
    }
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::standard(INVALID_REQUEST, ErrorCode::Invalid, message.into())
    }
    pub fn method_not_found(method: &str) -> Self {
        Self::standard(
            METHOD_NOT_FOUND,
            ErrorCode::NotFound,
            format!("there is no method {method}"),
        )
    }

    /// The error as it is carried inside other messages, such as a stream's close.
    pub fn body(&self) -> ErrorBody {
        ErrorBody {
            code: self.error_code(),
            message: self.message.clone(),
            details: self.details().cloned(),
        }
    }
}

impl From<ErrorBody> for RpcError {
    fn from(b: ErrorBody) -> Self {
        RpcError {
            code: b.code.number(),
            message: b.message,
            data: Some(ErrorData {
                code: b.code,
                details: b.details,
            }),
        }
    }
}

/// An error inside another message: a stream closed with an error, a failed job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
    pub details: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_its_own_number_and_name() {
        for code in ErrorCode::ALL {
            assert_eq!(ErrorCode::from_number(code.number()), code);
            let named: ErrorCode =
                serde_json::from_value(serde_json::json!(code.as_str())).unwrap();
            assert_eq!(named, code);
        }
        let error = RpcError::conflict("revision 3 is not 4");
        let text = serde_json::to_value(&error).unwrap();
        assert_eq!(text["data"]["code"], "CONFLICT");
        assert_eq!(RpcError::from(error.body()), error);
    }
}
