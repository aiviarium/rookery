//! Search, recent items, pins and recent activity: the methods behind the command palette, the search view,
//! the sidebar's pinned group and the home screen.

use serde::{Deserialize, Serialize};

use crate::data::EntityInfo;

// ── search ───────────────────────────────────────────────────────────────────────────────────────

/// `search.query`: ranked matches in titles and indexed text of what the caller may view. With no text,
/// what the caller opened last, then what changed last.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SearchParams {
    pub text: String,
    /// Only these schemes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub schemes: Option<Vec<String>>,
    /// Only entities in these spaces (ids).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub spaces: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub offset: Option<i64>,
    /// Also count the matches per scheme, ignoring `schemes`, for a type filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub facets: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub entity: EntityInfo,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub rank: f32,
    /// The title matched; otherwise the text indexed with the entity did.
    pub title_match: bool,
    /// The caller opened it lately.
    pub recent: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SearchFacet {
    pub scheme: String,
    /// At most 1000: a count of that many means "at least".
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub facets: Option<Vec<SearchFacet>>,
    /// More matches follow this page.
    pub more: bool,
}

// ── recents ──────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum RecentKind {
    Entity,
    Command,
}

impl RecentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RecentKind::Entity => "entity",
            RecentKind::Command => "command",
        }
    }
}

/// `recents.touch`: the user opened an entity (`key` is its URI) or ran a command (`key` is its id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RecentTouch {
    pub kind: RecentKind,
    pub key: String,
}

/// The result of `recents.touch`. An entity the user may not view is not recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RecentTouched {
    pub recorded: bool,
}

/// `recents.list`: the entities or the commands, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RecentsQuery {
    pub kind: RecentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub limit: Option<i64>,
    /// Entities only: these schemes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub schemes: Option<Vec<String>>,
    /// Entities only: this space (an id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub space: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct RecentEntity {
    pub entity: EntityInfo,
    pub opened_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct RecentCommand {
    pub id: String,
    pub opened_at: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub count: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RecentsList {
    pub entities: Vec<RecentEntity>,
    pub commands: Vec<RecentCommand>,
}

/// `recents.forget`: one entry, or every entry of the kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RecentForget {
    pub kind: RecentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub key: Option<String>,
}

// ── pins ─────────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum PinKind {
    Entity,
    View,
}

/// `pins.list`: the caller's pins and a space's shared ones (every space they can view when none is named).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PinsQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub space: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PinInfo {
    pub id: String,
    /// The space it is shared in; missing for a personal pin.
    pub space_id: Option<String>,
    pub kind: PinKind,
    /// An entity's URI, or a view's id.
    pub target: String,
    pub path: String,
    /// The space a pinned view opens in.
    pub view_space: Option<String>,
    pub title: String,
    pub icon: Option<String>,
    /// The pinned entity as the index knows it now.
    pub entity: Option<EntityInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PinList {
    pub pins: Vec<PinInfo>,
}

/// `pins.add`. Pinning the same thing twice returns the first pin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PinAdd {
    pub kind: PinKind,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub path: Option<String>,
    /// The space a pinned view opens in (an id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub view_space: Option<String>,
    /// Share the pin with this space's members (an id) instead of keeping it personal. Needs `use` on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub shared_in: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PinRemove {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PinRemoved {
    pub removed: bool,
}

/// The event `pins.changed`, to the user's windows (a personal pin) or the space's members (a shared one):
/// ask `pins.list` again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PinsChanged {
    pub space_id: Option<String>,
}

// ── activity ─────────────────────────────────────────────────────────────────────────────────────

/// `activity.recent`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ActivityQuery {
    /// Only this space (an id); every space the caller is in when missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub space: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ActivityItem {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub seq: i64,
    pub at: String,
    /// `insert`, `update`, `delete` or `restore`.
    pub op: String,
    pub actor_id: Option<String>,
    pub actor_name: Option<String>,
    pub entity: EntityInfo,
    /// The fields changed, where the collection keeps history.
    pub fields: Vec<String>,
    /// How many changes the entry stands for: a run of edits by one person is one entry.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub changes: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ActivityList {
    pub items: Vec<ActivityItem>,
}
