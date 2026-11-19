//! `auth.*`: who the caller is, their devices, access tokens and one-time sign-in links.

use serde::{Deserialize, Serialize};

use crate::context::{Principal, User};

/// `auth.me`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Me {
    /// The person; none over the Unix socket.
    pub user: Option<User>,
    pub principal: Principal,
    /// Whether the caller has the org role admin.
    pub admin: bool,
    /// How people sign in to this server: `local` or `oidc`.
    pub auth: AuthMode,
    pub version: String,
    pub api_version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// Local accounts, signed in by one-time links.
    Local,
    /// An OpenID Connect identity provider.
    Oidc,
}

/// One device session of the caller's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub label: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
    /// The session this call came with.
    pub current: bool,
}

/// `auth.listDevices`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DeviceList {
    pub devices: Vec<DeviceInfo>,
}

/// `auth.createToken`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TokenCreate {
    pub name: String,
    /// Days until it expires, 1 to 3650; it lasts until revoked without one.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub expires_in_days: Option<i64>,
}

/// An access token, without the token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TokenInfo {
    pub id: String,
    pub name: String,
    /// The last characters of the token, to tell tokens apart.
    pub hint: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
}

/// The result of `auth.createToken`: the token, shown this once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TokenCreated {
    /// `rookery_pat_…`.
    pub token: String,
    pub info: TokenInfo,
}

/// `auth.listTokens`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TokenList {
    pub tokens: Vec<TokenInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    /// Makes a new admin: the first one, from `rookery server setup-key`.
    Setup,
    /// Makes a new user with the name an admin gave.
    Invite,
    /// Signs an existing user in on another device.
    Device,
}

/// `auth.createLink`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkCreate {
    pub kind: LinkKind,
    /// The name of the person an invite makes.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub name: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub email: Option<String>,
    /// For a device link: whose. The caller's own without one.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub user_id: Option<String>,
}

/// A link that has not been used.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LinkInfo {
    pub id: String,
    pub kind: LinkKind,
    pub name: Option<String>,
    /// For a device link, the person it signs in.
    pub user_name: Option<String>,
    pub created_by_name: Option<String>,
    pub created_at: String,
    pub expires_at: String,
}

/// The result of `auth.createLink`: the link, shown this once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct LinkCreated {
    pub url: String,
    pub info: LinkInfo,
}

/// `auth.listLinks`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct LinkList {
    pub links: Vec<LinkInfo>,
}

/// What `GET /auth/session` answers a browser that is not signed in: how it can sign in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SignedOut {
    pub auth: AuthMode,
    /// Where to send the browser to sign in with the identity provider.
    pub login_url: Option<String>,
}
