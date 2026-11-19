//! Raw SQL on the wire: `sql.query`, `sql.queryStream`, `sql.execute`,
//! `sql.begin`, `sql.txQuery`, `sql.txExecute`, `sql.txPublish`, `sql.commit` and `sql.rollback`, for a plugin's
//! host half and, through `rookery.v1.Sql`, its services. A plugin has them only when its manifest says
//! `database.sql`.
//!
//! Parameters are positional (`$1`, `$2`, ...) and bound, never spliced. A JSON value is bound as the type
//! Postgres infers for its place: a number or string as the matching scalar, an array as an array, an
//! object as `jsonb`; bytes are `{"$bytes": "<base64>"}` and vectors `{"$vector": [..]}` (the SDK writes them
//! from `Uint8Array` and `Float32Array`). Results
//! are objects by column name: `numeric` is a string, timestamps are RFC 3339 strings in UTC, `json`/`jsonb`
//! are decoded, `bytea` is a base64 string (a CBOR byte string over gRPC) and `vector` an array of numbers
//! (a CBOR float32 array over gRPC).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `sql.execute` and the statement of `sql.txExecute` and `sql.txQuery`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SqlStatement {
    pub sql: String,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "Array<unknown>"))]
    pub params: Vec<Value>,
}

/// `sql.query` and `sql.queryStream`. With `replica`, the statement runs on the read replica
/// (`ROOKERY_DATABASE_READ_URL`) when the deployment has one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SqlQuery {
    pub sql: String,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "Array<unknown>"))]
    pub params: Vec<Value>,
    #[serde(default)]
    pub replica: bool,
}

/// A result column: its name and the Postgres type name (`int4`, `text`, `bytea`, `vector`, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SqlColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

/// The result of `sql.query`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SqlRows {
    pub columns: Vec<SqlColumn>,
    #[cfg_attr(feature = "ts", ts(type = "Array<Record<string, unknown>>"))]
    pub rows: Vec<Value>,
}

/// The result of `sql.execute` and `sql.txExecute`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SqlExecuted {
    /// Rows affected.
    pub rows: u64,
}

/// The result of `sql.queryStream`: the stream the rows arrive on, one JSON object per line, after which the
/// stream ends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SqlStreamed {
    pub stream: u32,
    pub columns: Vec<SqlColumn>,
}

/// The result of `sql.begin`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SqlTx {
    pub tx: String,
}

/// `sql.begin`: the options of a transaction. With `lease`, it commits only while the caller still holds that
/// lease with that token (docs/data.md, "Raw SQL"; docs/platform-split.md, "Leases / Fencing"); otherwise it is
/// rolled back and the commit is `CONFLICT` with `details.condition` `lease`, as a fenced batch gives.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SqlBegin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<SqlLease>,
}

/// A lease as a transaction's condition: the plugin's key and the token its grant returned. The holder is the
/// caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SqlLease {
    pub key: String,
    pub token: i64,
}

/// `sql.txQuery` and `sql.txExecute`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SqlTxStatement {
    pub tx: String,
    pub sql: String,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "Array<unknown>"))]
    pub params: Vec<Value>,
}

/// `sql.txPublish`: a topic message in a transaction (docs/data.md, "Durable topics / Publishing with a write").
/// The topic must be one the plugin declares and the payload must satisfy its schema when this call is made; the
/// message is written and numbered when the transaction commits, and not at all when it rolls back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SqlTxPublish {
    pub tx: String,
    pub topic: String,
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub payload: Value,
}

/// The result of `sql.txPublish` and the answer to a transaction stream's `publish`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SqlPublished {}

/// `sql.commit` and `sql.rollback`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SqlTxRef {
    pub tx: String,
}

/// The credential of a plugin with direct connections, for an admin placing a service somewhere the kernel
/// does not start it (`sql.credential`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SqlCredentialRef {
    pub plugin: String,
}

/// The result of `sql.credential` and `sql.rotateCredential`: the connection URL (it holds the password).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SqlCredential {
    pub url: String,
    pub role: String,
}
