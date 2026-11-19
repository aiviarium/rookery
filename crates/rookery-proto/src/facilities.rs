//! `settings.*`, `secrets.*`, `storage.*`, `connections.*`, `blobs.*` and `layout.*`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

macro_rules! wire {
    ($(#[$m:meta])* pub struct $name:ident { $($body:tt)* }) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
        #[cfg_attr(feature = "ts", derive(ts_rs::TS))]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        pub struct $name { $($body)* }
    };
}
pub(crate) use wire;

/// Where a setting's value is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum SettingScope {
    /// The plugin's own default.
    Default,
    /// For everyone, by an admin.
    Global,
    /// For the members of a space, named in `space`.
    Space,
    /// The caller's own.
    User,
}

/// Where a secret, a connection or a stored value belongs.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// For everyone.
    #[default]
    Global,
    /// A space's, named in `space`. Secrets have none.
    Space,
    /// The caller's own.
    User,
}

wire! {
    /// `settings.get`.
    pub struct SettingRef {
        pub plugin: String,
        pub key: String,
        /// The space of the `space` scope, or the space to resolve in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub space: Option<String>,
    }
}

wire! {
    /// A setting as it resolves for the caller.
    pub struct Setting {
        pub key: String,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub value: Value,
        /// Where the value comes from.
        pub scope: SettingScope,
    }
}

wire! {
    /// The result of `settings.get`: a value of `null`, and no scope, when the plugin has no value or
    /// default for the key.
    pub struct SettingValue {
        pub key: String,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub value: Value,
        pub scope: Option<SettingScope>,
    }
}

wire! {
    /// `settings.list`.
    pub struct SettingsOf {
        pub plugin: String,
        /// The space of the `space` scope, or the space to resolve in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub space: Option<String>,
    }
}

wire! {
    pub struct SettingList {
        pub entries: Vec<Setting>,
    }
}

wire! {
    /// `settings.set`: the default is the plugin's or an admin's to set, the global value an admin's,
    /// the user's value the caller's.
    pub struct SettingSet {
        pub plugin: String,
        pub key: String,
        pub scope: SettingScope,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub value: Value,
        /// The space of the `space` scope, or the space to resolve in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub space: Option<String>,
    }
}

wire! {
    /// `settings.reset`: removes the value at one scope.
    pub struct SettingReset {
        pub plugin: String,
        pub key: String,
        pub scope: SettingScope,
        /// The space of the `space` scope, or the space to resolve in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub space: Option<String>,
    }
}

wire! {
    /// `secrets.get` and `secrets.delete`.
    pub struct SecretRef {
        pub plugin: String,
        pub name: String,
        #[serde(default)]
        pub scope: Scope,
    }
}

wire! {
    /// `secrets.set`.
    pub struct SecretSet {
        pub plugin: String,
        pub name: String,
        #[serde(default)]
        pub scope: Scope,
        pub value: String,
    }
}

wire! {
    /// `secrets.rename`.
    pub struct SecretRename {
        pub plugin: String,
        pub name: String,
        #[serde(default)]
        pub scope: Scope,
        pub new_name: String,
    }
}

wire! {
    pub struct SecretValue {
        pub value: Option<String>,
    }
}

wire! {
    /// `secrets.list`.
    pub struct SecretsOf {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plugin: Option<String>,
    }
}

wire! {
    /// A secret without its value.
    pub struct SecretInfo {
        pub plugin: String,
        pub name: String,
        pub scope: Scope,
        /// The key it is sealed under.
        pub key_id: String,
        pub updated_at: String,
    }
}

wire! {
    pub struct SecretList {
        pub secrets: Vec<SecretInfo>,
    }
}

wire! {
    /// `storage.get` and `storage.delete`.
    pub struct StorageRef {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plugin: Option<String>,
        #[serde(default)]
        pub scope: Scope,
        pub key: String,
        /// For `storage.delete`: only at this revision.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub revision: Option<i64>,
        /// The space of the `space` scope, or the space to resolve in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub space: Option<String>,
    }
}

wire! {
    /// `storage.set`. With `revision`, only if the key is at it; 0 for a key that is not there yet.
    pub struct StorageSet {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plugin: Option<String>,
        #[serde(default)]
        pub scope: Scope,
        pub key: String,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub value: Value,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub revision: Option<i64>,
        /// The space of the `space` scope, or the space to resolve in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub space: Option<String>,
    }
}

wire! {
    /// `storage.list`: the keys that start with `prefix`, after `after`.
    pub struct StorageList {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plugin: Option<String>,
        #[serde(default)]
        pub scope: Scope,
        #[serde(default)]
        pub prefix: String,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub after: Option<String>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub limit: Option<i64>,
        /// The space of the `space` scope, or the space to resolve in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub space: Option<String>,
    }
}

wire! {
    pub struct StorageEntry {
        pub key: String,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub value: Value,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub revision: i64,
        pub updated_at: String,
    }
}

wire! {
    /// The result of `storage.get`: a value of `null` at revision 0 when nothing is stored.
    pub struct StorageValue {
        pub key: String,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub value: Value,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub revision: i64,
        pub updated_at: Option<String>,
    }
}

wire! {
    pub struct StorageEntries {
        pub entries: Vec<StorageEntry>,
    }
}

wire! {
    pub struct Revision {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub revision: i64,
    }
}

wire! {
    pub struct Deleted {
        pub deleted: bool,
    }
}

wire! {
    /// A type of connection: what its config holds, and whether it has a credential.
    pub struct ConnectionType {
        pub id: String,
        pub label: String,
        /// The JSON Schema of its config.
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub config: Value,
        /// What its credential is, such as "password"; none for a type without one.
        pub credential: Option<String>,
        /// Whether `connections.test` can try it.
        pub testable: bool,
    }
}

wire! {
    pub struct ConnectionTypes {
        pub types: Vec<ConnectionType>,
    }
}

wire! {
    /// A connection, without its credential.
    pub struct ConnectionInfo {
        pub id: String,
        #[serde(rename = "type")]
        #[cfg_attr(feature = "ts", ts(rename = "type"))]
        pub kind: String,
        pub name: String,
        pub scope: Scope,
        #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
        pub config: Value,
        pub has_credential: bool,
        pub updated_at: String,
        /// The space of a connection of the space scope.
        pub space_id: Option<String>,
    }
}

wire! {
    /// `connections.list`.
    pub struct ConnectionsOf {
        #[serde(default, rename = "type")]
        #[cfg_attr(feature = "ts", ts(optional, rename = "type"))]
        pub kind: Option<String>,
        /// The space of the `space` scope, or the space to resolve in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub space: Option<String>,
    }
}

wire! {
    pub struct ConnectionList {
        pub connections: Vec<ConnectionInfo>,
    }
}

wire! {
    /// `connections.create`: a global one is an admin's to make, a user's one the caller's.
    pub struct ConnectionCreate {
        #[serde(rename = "type")]
        #[cfg_attr(feature = "ts", ts(rename = "type"))]
        pub kind: String,
        pub name: String,
        #[serde(default)]
        pub scope: Scope,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
        pub config: Value,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub credential: Option<String>,
        /// The space of the `space` scope, or the space to resolve in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub space: Option<String>,
    }
}

wire! {
    /// `connections.update`: what is given changes. A credential of `""` removes it.
    pub struct ConnectionUpdate {
        pub id: String,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub name: Option<String>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional, type = "Record<string, unknown>"))]
        pub config: Option<Value>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub credential: Option<String>,
    }
}

wire! {
    /// `connections.resolve`: by id, or by type and name. Plugins resolve global connections.
    /// Admins can also resolve their own personal connections and connections in a named space.
    pub struct ConnectionResolve {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plugin: Option<String>,
        /// The space whose connection of a type and name to find.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub space: Option<String>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub id: Option<String>,
        #[serde(default, rename = "type")]
        #[cfg_attr(feature = "ts", ts(optional, rename = "type"))]
        pub kind: Option<String>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub name: Option<String>,
    }
}

wire! {
    /// A connection with its credential, for the code that uses it.
    pub struct ConnectionResolved {
        pub connection: ConnectionInfo,
        pub credential: Option<String>,
    }
}

wire! {
    /// The result of `connections.test`.
    pub struct ConnectionTested {
        pub ok: bool,
        pub message: String,
    }
}

wire! {
    /// `blobs.put`: up to 8 MiB in base64.
    pub struct BlobPut {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub name: Option<String>,
        pub content_type: String,
        pub data: String,
    }
}

wire! {
    pub struct BlobInfo {
        pub id: String,
        pub sha256: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub size: i64,
        pub content_type: String,
        pub name: Option<String>,
        pub created_at: String,
    }
}

wire! {
    /// `layout.get`.
    pub struct LayoutRef {
        /// `default` unless given.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub name: Option<String>,
    }
}

wire! {
    pub struct LayoutValue {
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub value: Option<Value>,
        /// 0 for a layout that was never put.
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub revision: i64,
    }
}

wire! {
    /// `layout.put`. With `revision`, only if the layout is at it.
    pub struct LayoutPut {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub name: Option<String>,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub value: Value,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub revision: Option<i64>,
    }
}
