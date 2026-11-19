//! Hosts as environments: `env.*`. Every method names its host by `env`, as `env://NAME` or the bare
//! name, and needs a level on `env://NAME`: `view` to see it, `use` for its files and ports as the
//! agent account.

use serde::{Deserialize, Serialize};

use crate::facilities::wire;

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
        /// The host's type: `ssh`, `managed`, `node`, `workstation` or `local`.
        #[cfg_attr(feature = "ts", ts(type = "\"ssh\" | \"managed\" | \"node\" | \"workstation\" | \"local\" | \"connector\" | (string & {})"))]
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
