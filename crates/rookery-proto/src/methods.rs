//! The method report: every method, the level it needs and its params, as checked in under `api/`.

use serde::{Deserialize, Serialize};

/// What a caller needs to call a method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Level {
    /// Anyone signed in, about their own things.
    #[serde(rename = "self")]
    Own,
    /// The org role admin.
    Admin,
    /// A level on the resource the param `param` names, as a URI such as `env://studio`.
    Resource { param: String, grant: Grant },
    /// A plugin for itself, or an admin naming the plugin in `plugin`.
    Plugin,
}

/// A level on a resource. Each includes those before it.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Grant {
    View = 1,
    Use = 2,
    Manage = 3,
}

impl Grant {
    pub fn from_number(n: i16) -> Option<Grant> {
        match n {
            1 => Some(Grant::View),
            2 => Some(Grant::Use),
            3 => Some(Grant::Manage),
            _ => None,
        }
    }
}

/// One method in the report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MethodInfo {
    pub name: String,
    pub summary: String,
    pub level: Level,
    /// Whether the result is a stream.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
    pub params: serde_json::Value,
    pub result: serde_json::Value,
}
