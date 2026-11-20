//! Hosts as environments: `env.*`. Every method names its host by `env`, as `env://NAME` or the bare
//! name, and needs a level on `env://NAME`: `view` to see it, `use` for its files and ports as the
//! agent account.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::facilities::wire;
use crate::terminals::EnvUser;

wire! {
    /// `{ env }`: a host as `env://NAME` or its name.
    pub struct EnvRef {
        pub env: String,
    }
}

/// `online`, `offline`, `starting`, `degraded`, with the reason in `error` for the last three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum EnvState {
    Online,
    Offline,
    Starting,
    Degraded,
}

wire! {
    /// What a host can do, from its type and what its controller was seen to be.
    pub struct EnvSupports {
        pub terminal: bool,
        pub process: bool,
        pub files: bool,
        pub connect: bool,
        /// Whether the host runs a Parrot broker (`agents.connect`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub agents: Option<bool>,
        pub checkouts: bool,
        /// A desktop on a virtual display (VNC on the host's loopback).
        pub desktop: bool,
        /// Sessions as `operator` or `root`, which need manage.
        pub privileged: bool,
        /// Sessions outlive the controller's connection and can be attached again.
        pub persistent_sessions: bool,
        /// Plugins' app services can be placed on it.
        pub deploy: bool,
    }
}

wire! {
    /// `env.list`, `env.get`: a host as an environment.
    pub struct EnvInfo {
        pub name: String,
        /// `env://<name>`.
        pub uri: String,
        /// The host's type: `ssh`, `managed`, `node` or `workstation`.
        #[cfg_attr(feature = "ts", ts(type = "\"ssh\" | \"managed\" | \"node\" | \"workstation\" | (string & {})"))]
        pub kind: String,
        pub state: EnvState,
        /// The owner's user id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub owner: Option<String>,
        /// The caller's level on it: `view`, `use` or `manage`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub level: Option<crate::methods::Grant>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub os: Option<String>,
        /// The version of its controller.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub version: Option<String>,
        pub supports: EnvSupports,
        /// Why it is offline, starting or degraded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub error: Option<String>,
    }
}

wire! {
    pub struct EnvList {
        pub environments: Vec<EnvInfo>,
    }
}

wire! {
    /// The event `rookery/env.changed`: a host's state, type or access changed, or it was removed.
    /// `environment` is as `env.get` gives it to the subscriber, and absent once it is removed.
    pub struct EnvChanged {
        /// `env://<name>`.
        pub env: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub environment: Option<EnvInfo>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub removed: bool,
    }
}

wire! {
    /// One person with terminals running on a host.
    pub struct PresenceUser {
        /// The user's id.
        pub id: String,
        pub name: String,
        /// Their terminals that run on the host.
        pub terminals: u32,
        /// The newest start among them, RFC 3339.
        pub since: String,
    }
}

wire! {
    /// `env.presence`: who is on a host. Terminals are their owner's, so this is how someone with
    /// `view` learns that another person works there, without seeing what they do.
    pub struct EnvPresence {
        pub users: Vec<PresenceUser>,
    }
}

wire! {
    /// `env.readFile`, `env.listDir`, `env.stat`: a path on the host, inside its roots.
    pub struct FilePath {
        pub env: String,
        /// Absolute, inside the host's roots.
        pub path: String,
    }
}

wire! {
    /// The result of `env.readFile`.
    pub struct FileData {
        pub base64: String,
        pub size: u64,
        /// SHA-256 of the content, lower-case hex: the version `env.writeFile` can name in `ifMatch`.
        pub sha256: String,
    }
}

wire! {
    /// `env.writeFile`. Missing folders are made.
    pub struct EnvFileWrite {
        pub env: String,
        pub path: String,
        pub base64: String,
        /// Replace a file that is there; otherwise one that is there is a `CONFLICT`.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub overwrite: bool,
        /// Write only while the file's SHA-256 is this, as `env.readFile` gave it; otherwise, or
        /// when the file is gone, `CONFLICT`. Implies `overwrite`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub if_match: Option<String>,
    }
}

wire! {
    /// The result of `env.writeFile`.
    pub struct FileWritten {
        /// SHA-256 of what was written, the file's version from now on.
        pub sha256: String,
        pub size: u64,
    }
}

wire! {
    /// `env.deleteFile`.
    pub struct FileDelete {
        pub env: String,
        pub path: String,
        /// A folder with what is in it.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub recursive: bool,
    }
}

/// What a path names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    File,
    Dir,
    Link,
    Other,
}

wire! {
    pub struct DirEntry {
        pub name: String,
        pub kind: FileKind,
        pub size: u64,
        /// RFC 3339, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub modified: Option<String>,
    }
}

wire! {
    /// The result of `env.listDir`.
    pub struct DirListing {
        pub path: String,
        pub entries: Vec<DirEntry>,
    }
}

wire! {
    /// The result of `env.stat`: `entry` is absent when nothing is there.
    pub struct FileStat {
        pub path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub entry: Option<DirEntry>,
    }
}

wire! {
    /// `env.exec`: runs a command on a host to its end and answers with its output. As the agent
    /// account, or as `operator` or `root` with manage.
    pub struct ExecParams {
        pub env: String,
        /// The words of the command: a program and its arguments, not a shell line.
        pub command: Vec<String>,
        /// An absolute folder inside the host's roots. Default: the controller's own folder,
        /// which is the agent account's home on a host Rookery set up.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub cwd: Option<String>,
        /// Variables for the command, which the host adds to the agent's own environment.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        #[cfg_attr(feature = "ts", ts(as = "Option<BTreeMap<String, String>>", optional))]
        pub vars: BTreeMap<String, String>,
        /// Text written to standard input, which is then closed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub stdin: Option<String>,
        /// `agent` unless the caller has manage on the host.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<EnvUser>", optional))]
        pub user: EnvUser,
        /// Ends the command after this long, as `TIMEOUT`. Default 60 s, at most an hour.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub timeout_ms: Option<u64>,
    }
}

wire! {
    /// The result of `env.exec`. It carries what fits the budget of a message, and the start of
    /// the rest is left out when `truncated` says so.
    pub struct ExecResult {
        /// The exit code, absent when the command was killed without one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub exit_code: Option<i32>,
        /// The command ended with the host's controller, not by itself.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub lost: bool,
        /// It wrote more than the result carries, or the host dropped what it wrote before this
        /// call could read it, so `stdout` and `stderr` are not all of it.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub truncated: bool,
        /// As text, invalid UTF-8 replaced.
        pub stdout: String,
        pub stderr: String,
    }
}

wire! {
    /// A clone of a repository on one host, or a git worktree of one, under the host's checkouts
    /// folder.
    pub struct Checkout {
        /// `env://<name>`.
        pub env: String,
        /// The checkout's absolute path.
        pub path: String,
        /// Its folder name under the checkouts folder.
        pub name: String,
        /// Its current branch, absent when its HEAD is detached.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub branch: Option<String>,
        /// The commit its HEAD names.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub head: Option<String>,
        /// `origin`'s URL.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub url: Option<String>,
        /// For a worktree: the checkout it belongs to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub worktree_of: Option<String>,
    }
}

wire! {
    /// The result of `env.checkouts`.
    pub struct CheckoutList {
        pub checkouts: Vec<Checkout>,
    }
}

wire! {
    /// `env.clone`: clones a repository into `<checkouts folder>/<name>`.
    pub struct CheckoutCreate {
        pub env: String,
        /// The address git clones, such as `https://example.test/acme/widgets.git`.
        pub repository: String,
        /// Default: the last segment of the address without `.git`, with anything else as `-`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub branch: Option<String>,
    }
}

wire! {
    /// `env.worktree`: a git worktree of a checkout, on a new branch, at
    /// `<checkouts folder>/<name>`.
    pub struct WorktreeCreate {
        pub env: String,
        /// The checkout's path, inside the host's checkouts folder.
        pub checkout: String,
        /// The branch to make, in the checkout.
        pub branch: String,
        /// What the branch starts from. Default: the checkout's HEAD.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub base: Option<String>,
        /// Default: `<checkout name>-<branch>`, with `/` as `-`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub name: Option<String>,
    }
}

wire! {
    /// `env.removeCheckout`: removes a checkout, or a worktree through `git worktree remove`.
    pub struct CheckoutRemove {
        pub env: String,
        pub path: String,
        /// Remove it with its changes.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub force: bool,
    }
}

wire! {
    /// `env.connect`: a byte stream to a TCP port on the host's loopback. `streams.eof` half-closes.
    pub struct Connect {
        pub env: String,
        pub port: u16,
    }
}

wire! {
    /// `env.portUrl`: where a browser opens a port of a host.
    pub struct PortRef {
        pub env: String,
        pub port: u16,
        /// The path under the port, `/` when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub path: Option<String>,
    }
}

wire! {
    /// The result of `env.portUrl`: an address on the ports' own origin that signs the browser in to
    /// that one port once, within a minute, and then shows `path`.
    pub struct PortUrl {
        pub url: String,
        /// The origin the port is served on.
        pub origin: String,
    }
}
