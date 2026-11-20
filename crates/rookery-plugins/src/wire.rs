//! The params and results of `plugins.*`, and the notifications `plugins.changed` and
//! `plugins.reloaded`. The client's `PluginInfo`, `PluginList`, `ClientInfo`, `PluginChanged` and
//! `PluginReloaded` in `client/src/generated/` have the same shape.

use rookery_proto::ErrorBody;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Where a plugin is in its life on this server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PluginState {
    /// Its host half has not started: it starts when something it contributes is used.
    Inactive,
    Activating,
    /// Its host half runs, or it has none.
    Active,
    /// Its host half failed to start, or ended too often.
    Failed,
    /// Its manifest is not valid, or Rookery cannot use it.
    Invalid,
}

/// The client half as a shell loads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    /// `/plugins/<id>/<hash>/<path>`: the files of a version are served at an address of their own.
    pub script: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub styles: Option<String>,
    /// The SHA-256 of the client half's files, in hex.
    pub hash: String,
}

/// A plugin installed on the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub state: PluginState,
    /// Why it is invalid or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
    /// Its `plugin.json`, with its id and version from `package.kdl` and every list a client reads.
    /// Missing when it is not valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientInfo>,
    /// Its package, `GROUP/ID`.
    pub package: String,
    pub version: String,
    /// `shipped`, `registry`, `git` or `file`.
    pub origin: String,
    /// The registry, repository or file it was installed from.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// It still has a `rookery.json`, read as a `plugin.json` with host features and provisioners.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub legacy: bool,
    /// Its name, from `package.kdl`, and its summary.
    #[serde(default)]
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Where its package is on the server.
    #[serde(default)]
    pub dir: String,
    /// Why Rookery does not use it, as `error` says, in a sentence.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub problem: String,
    /// Its host features and provisioners as the host core reads them, with their actions, services
    /// and channels: what `rookery plugin list` and `show` print.
    #[serde(default)]
    pub features: Vec<rookery_api::PluginFeature>,
    #[serde(default)]
    pub provisioners: Vec<rookery_api::ProvisionerInfo>,
    /// When this server first recorded it, and when its version or files last changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// `plugins.list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PluginList {
    pub plugins: Vec<PluginInfo>,
    /// Plugins the database records as installed that the plugin folder does not have, as on a new
    /// leader whose folder is behind. Their records stay until the next change of the plugins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
}

/// `plugins.add`: a package of the plugin registries, the https address of an egg, or one folder of a
/// git repository.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PluginAdd {
    /// `GROUP/ID[@RANGE]`, `ID[@RANGE]`, or the https address of an egg. Empty with `git`.
    #[serde(default)]
    pub spec: String,
    /// A git repository to install one folder of, instead of `spec`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<String>,
    /// With `git`: the package's folder in the repository; its root without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// With `git`: a tag, branch or commit; the default branch without one.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    /// Install a version even when it is younger than the registry's minimum release age.
    #[serde(default)]
    pub allow_young: bool,
}

/// A plugin by its id or package.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PluginRef {
    pub id: String,
}

/// `plugins.update`: one plugin by its id or package, or all of them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PluginUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// What a change of the plugins did, a few lines, and the plugins after it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PluginReport {
    pub lines: Vec<String>,
    pub plugins: Vec<PluginInfo>,
    /// After an update: the saved hosts whose configuration is now behind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub behind: Vec<rookery_api::HostBehind>,
}

/// `plugins.upload`: an egg the caller sends, in base64.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PluginUpload {
    pub base64: String,
}

/// `plugins.registry.remove`: a registry by its location or name.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RegistryLocation {
    pub location: String,
}

/// Whether a newer version of a plugin is there, as its registry says.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PluginNewer {
    pub id: String,
    pub package: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// `plugins.check`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PluginUpdates {
    pub plugins: Vec<PluginNewer>,
}

/// `plugins.rpc`: a method a plugin's host half registered with `rookery.rpc.handle`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PluginRpc {
    pub plugin: String,
    pub method: String,
    #[serde(default = "empty")]
    pub params: Value,
}

fn empty() -> Value {
    Value::Object(Default::default())
}

/// Notification `plugins.changed`: a plugin was added, changed or removed.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PluginChanged {
    pub id: String,
    /// `null` when it was removed.
    pub plugin: Option<PluginInfo>,
}

/// Notification `plugins.reloaded`: the plugin folder was read again, and this is what the plugin is
/// now. A shell re-imports the client half when `hash` changed and remounts the plugin's views.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PluginReloaded {
    pub id: String,
    /// The SHA-256 of the client half the server serves now, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}
