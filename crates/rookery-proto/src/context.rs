//! Who a call is from.

use serde::{Deserialize, Serialize};

/// A person known to the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct User {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub email: Option<String>,
    pub admin: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum PrincipalKind {
    /// A person, by a device session or an access token.
    User,
    /// The server's own machine, over the Unix socket: an admin with no user.
    Local,
    /// Code inside the server, such as a job handler.
    System,
    /// A plugin, acting for itself.
    Plugin,
}

/// Who acts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Principal {
    pub kind: PrincipalKind,
    pub id: String,
}

impl Principal {
    /// How grants and the audit log name it: `user:<id>`, `local:socket`, `system:jobs`, `plugin:<id>`.
    pub fn uri(&self) -> String {
        let kind = match self.kind {
            PrincipalKind::User => "user",
            PrincipalKind::Local => "local",
            PrincipalKind::System => "system",
            PrincipalKind::Plugin => "plugin",
        };
        format!("{kind}:{}", self.id)
    }
}
