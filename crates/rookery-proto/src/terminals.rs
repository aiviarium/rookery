//! Terminals: `terminals.*`. A terminal is a session on a host that the server keeps a record of: whose
//! it is, its size, its viewers and how far its output has come. The screen stays the host's.
//!
//! A terminal is its owner's. `use` on the host opens terminals and drives one's own; another
//! account's terminal needs `manage` on the host. `view` sees none of them, only how many each person
//! runs (`env.presence`).
//!
//! `terminals.attach` opens a stream. Each frame, both ways, is one type byte and a body:
//!
//! | Type | From the server | From the viewer |
//! | --- | --- | --- |
//! | `0` | output, counted in the terminal's offset | input, as typed |
//! | `1` | a JSON [`TerminalEvent`] | a JSON [`AttachControl`] |
//! | `2` | the screen as it stands, not counted, before any `0` | — |

use serde::{Deserialize, Serialize};

use crate::facilities::wire;
use crate::methods::Grant;

/// The first frame byte of output, and of a viewer's input.
pub const FRAME_OUTPUT: u8 = 0;
/// The first frame byte of an event, and of a viewer's control.
pub const FRAME_EVENT: u8 = 1;
/// The first frame byte of the screen sent before output.
pub const FRAME_SNAPSHOT: u8 = 2;

wire! {
    pub struct TermSize {
        pub cols: u16,
        pub rows: u16,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum TerminalState {
    Running,
    Exited,
    /// Its session ended with the host or its controller, not by itself.
    Lost,
}

/// The account a terminal runs as. Anything but `agent` needs manage on the host.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum EnvUser {
    #[default]
    Agent,
    Operator,
    Root,
}

impl EnvUser {
    pub fn as_str(self) -> &'static str {
        match self {
            EnvUser::Agent => "agent",
            EnvUser::Operator => "operator",
            EnvUser::Root => "root",
        }
    }
}

wire! {
    /// Who last typed into a terminal.
    pub struct InputInfo {
        /// `user:<id>` or another principal.
        pub principal: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub name: Option<String>,
        /// Unix ms.
        pub at: i64,
        /// `viewer`, from an attached viewer, or `automation`, from `terminals.write`.
        pub via: String,
        pub bytes: u64,
    }
}

wire! {
    pub struct TerminalInfo {
        pub id: String,
        /// `terminal://<id>`.
        pub uri: String,
        /// `env://<name>`.
        pub env: String,
        pub title: String,
        pub command: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub cwd: Option<String>,
        /// The owner's user id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub owner: Option<String>,
        pub user: EnvUser,
        pub state: TerminalState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub exit_code: Option<i32>,
        pub size: TermSize,
        /// Viewers attached now.
        pub viewers: u32,
        /// Output bytes so far, as far as the server has seen.
        pub offset: u64,
        /// RFC 3339.
        pub created_at: String,
        /// The session's id on the host.
        pub session_id: String,
        /// Recordings come later; always false.
        pub recording: bool,
        /// The caller's level on it: `use` for one's own, `manage` for another's on a host one
        /// manages.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub level: Option<Grant>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub last_input: Option<InputInfo>,
    }
}

wire! {
    /// `terminals.open`: a terminal on a host, at its default shell when there is no command.
    pub struct TerminalOpen {
        pub env: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
        pub command: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub title: Option<String>,
        /// Environment variables for the command.
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        #[cfg_attr(feature = "ts", ts(as = "Option<std::collections::BTreeMap<String, String>>", optional))]
        pub vars: std::collections::BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub size: Option<TermSize>,
        /// `agent` when absent; `operator` and `root` need manage on the host.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<EnvUser>", optional))]
        pub user: EnvUser,
    }
}

wire! {
    /// `terminals.get`, `terminals.close`: a terminal by its id.
    pub struct TerminalRef {
        pub id: String,
    }
}

wire! {
    /// `terminals.list`: the caller's terminals, newest first.
    pub struct TerminalQuery {
        /// Only those on this host.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub env: Option<String>,
        /// Also the last 50 that ended.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub ended: bool,
        /// Also other people's, on the hosts the caller manages.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub all: bool,
    }
}

wire! {
    pub struct TerminalList {
        pub terminals: Vec<TerminalInfo>,
    }
}

wire! {
    /// `terminals.attach`.
    pub struct TerminalAttach {
        pub id: String,
        /// Go on after this output offset. Absent on a first attach, which starts with the screen.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub after: Option<u64>,
        /// The viewer's size, for the size rule.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub size: Option<TermSize>,
        /// Watch only: the viewer's input and size are ignored.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub readonly: bool,
    }
}

/// How an attach starts: with the screen as it stands, or with the output after `after`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum AttachMode {
    /// Clear the display: type-2 frames draw the screen, then output goes on from `offset`.
    Snapshot,
    /// Output goes on from `offset`, which is the `after` asked for.
    Resume,
}

wire! {
    /// The result of `terminals.attach`: the stream and how it starts.
    pub struct TerminalAttached {
        pub stream: u32,
        pub mode: AttachMode,
        /// The output offset the first type-0 frame starts at.
        pub offset: u64,
        /// Bytes of type-2 frames that come before it.
        pub snapshot: u64,
        /// The terminal's size now.
        pub size: TermSize,
        /// This viewer's id, which a `size` event's `active` names.
        pub viewer: String,
        /// Whether the viewer's input is ignored.
        pub readonly: bool,
        pub info: TerminalInfo,
    }
}

/// A type-1 frame from the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TerminalEvent {
    /// The terminal's size, and the viewer it follows.
    Size {
        cols: u16,
        rows: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        active: Option<String>,
    },
    Title {
        title: String,
    },
    State {
        state: TerminalState,
        #[serde(default, skip_serializing_if = "Option::is_none", rename = "exitCode")]
        #[cfg_attr(feature = "ts", ts(optional))]
        exit_code: Option<i32>,
    },
    Viewers {
        count: u32,
    },
    /// The replay is done; what follows is live.
    Ready,
}

/// A type-1 frame from a viewer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AttachControl {
    /// The viewer's area fits this size.
    Resize { cols: u16, rows: u16 },
    /// The viewer is in use: the terminal takes its size.
    Active,
}

wire! {
    /// `terminals.write`: input for a terminal, as text or as bytes in base64.
    pub struct TerminalWrite {
        pub id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub base64: Option<String>,
    }
}

wire! {
    /// `terminals.resize`: the size, as an active viewer would set it.
    pub struct TerminalResize {
        pub id: String,
        pub size: TermSize,
    }
}

wire! {
    /// `terminals.signal`: `INT`, `TERM`, `HUP` or `KILL`.
    pub struct TerminalSignal {
        pub id: String,
        pub signal: String,
    }
}

wire! {
    /// `terminals.rename`.
    pub struct TerminalRename {
        pub id: String,
        pub title: String,
    }
}

wire! {
    /// `terminals.screen`.
    pub struct ScreenQuery {
        pub id: String,
        /// Up to this many lines of scrollback before the screen.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub history: Option<u32>,
        /// The screen as escape sequences in `ansi`.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub ansi: bool,
    }
}

wire! {
    pub struct CursorInfo {
        pub row: u16,
        pub col: u16,
    }
}

wire! {
    /// The result of `terminals.screen`: the screen as the host keeps it.
    pub struct ScreenInfo {
        pub size: TermSize,
        /// One string per row; with `history`, the scrollback lines come first.
        pub lines: Vec<String>,
        pub cursor: CursorInfo,
        /// The output offset the server had seen when it asked.
        pub offset: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub ansi: Option<String>,
    }
}

wire! {
    /// `terminals.sessions`.
    pub struct EnvSessionQuery {
        pub env: String,
        /// The last lines of each session's screen, for one the caller may adopt.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
        pub preview: bool,
    }
}

wire! {
    /// A session on a host.
    pub struct EnvSession {
        pub id: String,
        /// `terminal` or `process`.
        pub kind: String,
        pub title: String,
        pub command: Vec<String>,
        pub user: String,
        pub started: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub ended: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub exit_code: Option<i32>,
        pub lost: bool,
        /// The terminal it is, when it is one the caller may see.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub terminal: Option<String>,
        /// Whether it is already a terminal, the caller's or not.
        pub adopted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub preview: Option<String>,
    }
}

wire! {
    pub struct EnvSessionList {
        pub sessions: Vec<EnvSession>,
    }
}

wire! {
    /// `terminals.adopt`: a terminal session of a host that no terminal holds, made a terminal of the
    /// caller's.
    pub struct TerminalAdopt {
        pub env: String,
        pub session: String,
    }
}

wire! {
    /// The event `rookery/terminals.changed`: a terminal's title, state, size or viewers changed. Sent
    /// to those who may see it, with their level.
    pub struct TerminalChanged {
        pub terminal: TerminalInfo,
    }
}
