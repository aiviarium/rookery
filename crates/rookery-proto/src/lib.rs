//! The wire types of `rookery serve`.
//!
//! Every type that crosses the edge (`/ws`, `/rpc`, the Unix socket) is here. JSON field names are
//! camelCase. Each derives a JSON Schema for the method report in `api/`, and with the feature `ts` a
//! TypeScript type for `client/src/generated/`.

pub mod access;
pub mod auth;
pub mod context;
pub mod error;
pub mod facilities;
pub mod hosts;
pub mod jobs;
pub mod methods;
pub mod rpc;
pub mod stream;

pub use context::{Principal, PrincipalKind, User};
pub use error::{ErrorBody, ErrorCode, ErrorData, RpcError};
pub use rpc::{Id, Message, Notification, Request, Response};

/// The version of the API this server speaks.
pub const API_VERSION: u32 = 1;

/// Params or a result with nothing in it: `{}`.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct Empty {}

/// A thing named by its id.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct IdRef {
    pub id: String,
}
