//! The wire types of hosts and of the rest of Rookery's own: `hosts.*`, `keys.*`, `provisioners.*`,
//! `resources.*`, `installation.*`, `setup.*` and `plugins.*`. Their field names are snake_case, as the
//! types of `rookery-api` they carry, which the host protocol shares.

use std::collections::BTreeMap;

use rookery_api::{
    Act, Action, Behind, HostType, Power, ResourceChange, SavedProvisioner, Session,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::jobs::{JobInfo, JobState};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostInfo {
    /// Its id, which stays when it is renamed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub name: String,
    /// Its SSH address.
    pub address: String,
    pub reachable: bool,
    /// Whether the host advertises a Parrot broker.
    #[serde(default)]
    pub agents: bool,
    /// The version of its rookeryd.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<u32>,
    /// Why it is not reachable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The rookery version its configuration was last applied with, when its
    /// rookeryd says, which it does for a host applied since.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied: Option<String>,
    /// What changed since then, when the server would apply something else now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<Behind>,
    /// The type its profile means it to be.
    #[serde(rename = "type")]
    pub host_type: HostType,
    /// The type it was seen to be: what its controller's record says, `ssh`
    /// when rookery logged in and found no controller. Absent when it could
    /// not be asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seen: Option<HostType>,
    /// A rotation of a key it logs in with is not done on it yet.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub rotating: bool,
    /// Whether rookery can start and stop it, and whether it runs: running
    /// when its controller answers, else as its provisioner said last.
    pub power: Power,
    /// The machine a provisioner made for it, or one planned for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine: Option<HostMachine>,
}

crate::facilities::wire! {
    /// A host as an environment, named by `env://NAME` or its bare name.
    pub struct AgentConnect {
        pub env: String,
    }
}

/// The machine of a host, as rookery's record of the resource has it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostMachine {
    /// rookery's id of the resource.
    pub resource: String,
    /// The provisioner that made it, or is to make it.
    pub provisioner: String,
    /// The provisioner's id of it; empty until it is made.
    pub external_id: String,
    /// As last seen, or `planned` for one not asked for yet.
    pub state: String,
}

/// A key of the ring, by its public half.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct KeyInfo {
    /// Its id; empty for the key that replaces another while it rotates.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub name: String,
    /// The public half, a line of `authorized_keys`.
    pub public: String,
    /// Its SHA256 fingerprint, as `ssh-keygen -l` shows it.
    pub fingerprint: String,
    /// The key replacing it while a rotation runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<Box<KeyInfo>>,
    /// The server's hosts that log in with it.
    #[serde(default)]
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct KeyList {
    pub keys: Vec<KeyInfo>,
}

// ── Params and results ──────────────────────────────────────────────────────

/// A host by its name, or as `env://NAME`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostRef {
    pub host: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostList {
    pub hosts: Vec<HostInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostSave {
    pub host: String,
    /// The whole profile. A workstation's has a builder object; a host of the type ssh or managed
    /// has `type` and `access` alone.
    pub profile: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostSaved {
    pub host: HostInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostRename {
    pub host: String,
    pub to: String,
    /// Give the machine the new name as its hostname too, at its next apply.
    #[serde(default)]
    pub hostname: bool,
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostProfile {
    pub profile: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostCheck {
    pub host: String,
    /// What it is to do, such as `apply`, `sessions` or `desktop`.
    pub capability: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Refusal {
    pub why: String,
    /// The action that raises the host's type so it can: `install-controller`, `apply` or `setup`.
    pub raise: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostChecked {
    pub allowed: bool,
    pub refusal: Option<Refusal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ActionList {
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostAct {
    pub host: String,
    pub action: String,
    #[serde(flatten)]
    pub act: Act,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct InstallationAct {
    pub action: String,
    #[serde(flatten)]
    pub act: Act,
}

/// What running an action came to: the job that runs it, by its id, and its state; the session on the
/// host for an action the host runs in a terminal; or what an action that answers at once said.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Acted {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<JobState>,
    /// The whole job, as `jobs.get` gives it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<JobInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<Session>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

/// A call of a host's controller: the path under `/v1` and its query.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostQuery {
    pub host: String,
    /// The query, as the controller takes it, such as `unit` and `lines` of the logs.
    #[serde(default)]
    pub query: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostPower {
    pub host: String,
    /// `reboot`, `poweroff` or `suspend`.
    pub verb: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostUnit {
    pub host: String,
    pub unit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostUnitAct {
    pub host: String,
    pub unit: String,
    /// `start`, `stop`, `restart`, `enable` or `disable`.
    pub verb: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostSignal {
    pub host: String,
    pub pid: u32,
    #[serde(default)]
    pub signal: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostService {
    pub host: String,
    pub plugin: String,
    pub name: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostServiceStream {
    pub host: String,
    pub plugin: String,
    pub name: String,
}

/// What a host's controller answered: its JSON, or bytes as `base64` with their `content_type`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(transparent)]
pub struct Passed(pub Value);

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct KeyRef {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct KeyAdd {
    pub name: String,
    /// The private half in OpenSSH's format; left out, a new key is made.
    #[serde(default)]
    pub private: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Named {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ProvisionerSave {
    pub name: String,
    #[serde(flatten)]
    pub saved: SavedProvisioner,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ProvisionerGet {
    pub name: String,
    /// Values of its arguments to ask it with, such as a server it would use.
    #[serde(default)]
    pub given: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ProvisionerAct {
    pub name: String,
    pub action: String,
    #[serde(flatten)]
    pub act: Act,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ResourceRef {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ResourceAct {
    pub id: String,
    pub action: String,
    #[serde(flatten)]
    pub act: Act,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ResourceChangeOf {
    pub id: String,
    #[serde(flatten)]
    pub change: ResourceChange,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ResourceRemove {
    pub id: String,
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ResourceLogs {
    pub id: String,
    /// The last so many lines; all when 0 (default 100).
    #[serde(default)]
    pub tail: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Text {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PluginRef {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostOnly {
    #[serde(default)]
    pub host: Option<String>,
}

impl Acted {
    /// For a job that runs it.
    pub fn of_job(job: JobInfo) -> Acted {
        Acted {
            job: Some(job.id.clone()),
            state: Some(job.state),
            info: Some(job),
            session: None,
            result: None,
        }
    }
}

/// `hosts.sessions.open`: a session on the host, a terminal or a process. As the SSH user or root it
/// needs manage on the host; as the agent account, use.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostSessionOpen {
    pub host: String,
    #[serde(flatten)]
    pub start: rookery_api::StartSession,
    /// How long to wait for it to be running, such as `20s`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<String>,
}

/// A session on a host, by the host and the session's id.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostSessionRef {
    pub host: String,
    pub terminal: String,
    /// The query, as the host takes it: `since`, `tail`, `lines` and the rest.
    #[serde(default)]
    pub query: BTreeMap<String, String>,
}

/// `hosts.sessions.input`, `hosts.sessions.resize` and `hosts.sessions.signal`: what the host takes, as it takes it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostSessionSend {
    pub host: String,
    pub terminal: String,
    #[serde(default)]
    pub body: Value,
}

/// A file on a host, by its path.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostFileRef {
    pub host: String,
    pub path: String,
}

/// `hosts.files.write`: the bytes in base64. With `etag`, only if the file is still at it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostFileWrite {
    pub host: String,
    pub path: String,
    pub base64: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
}

/// `hosts.ports.connect`: a TCP port on the host's loopback.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostPortConnect {
    pub host: String,
    pub port: u16,
}

/// `resources.connect`: an endpoint a resource's provisioner reported, by its name or its port.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EndpointConnect {
    pub id: String,
    pub endpoint: String,
}

/// A secret, a registry or a setting of the installation, by its name, with what changes it.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NamedValue {
    pub name: String,
    /// The secret's or setting's value, the registry as JSON, or a new name.
    #[serde(default)]
    pub value: Value,
}

/// `hosts.messages.list`: the messages taken in from the hosts' channels that they keep, newest first.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MessageQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Those whose topic starts with this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<bool>,
    /// Those taken in before this `seq`: the page after one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// A message a host kept, with where it came from.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostMessage {
    #[serde(flatten)]
    pub message: rookery_api::Message,
    pub host: String,
    pub done: bool,
    /// The order it was taken in.
    pub seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MessageList {
    pub messages: Vec<HostMessage>,
    /// `before` for older ones, when there are more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<i64>,
}

/// A host's message by its id there.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MessageRef {
    pub host: String,
    pub id: u64,
}

/// `hosts.inventory`: hosts by name, or every host with an address when there are none.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostNames {
    #[serde(default)]
    pub hosts: Vec<String>,
}

/// An Ansible inventory, with the server's key as a file of the server's machine.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Inventory {
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
    pub inventory: serde_json::Value,
}

/// `hosts.ssh`: a plain SSH login to a host, as the operator or, with `agent`, as its agent.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostSsh {
    pub host: String,
    #[serde(default)]
    pub agent: bool,
}

/// The `ssh` command of a login, without the remote command, and its environment.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SshCommand {
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// `hosts.askpass`: what ssh asks rookery's askpass helper, and where the password is kept.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AskpassAsked {
    pub prompt: String,
    /// `secret:NAME`, as the helper's environment says it.
    pub source: String,
}

/// The password to answer with.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AskpassAnswer {
    pub answer: String,
}

/// A one-use enrollment key for a host's tailnet, which an apply asks for.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TailnetKey {
    pub key: String,
}
