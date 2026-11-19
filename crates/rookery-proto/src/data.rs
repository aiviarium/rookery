//! Params and results of the data facilities: spaces, access and sharing, collections, live queries and
//! the change feed, entities and references. Rows are JSON objects shaped by the collection's fields plus
//! the system fields (`id`, `spaceId`, `createdAt`, `rev`, ...).

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── spaces ───────────────────────────────────────────────────────────────────────────────────────

/// A space as the caller sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SpaceInfo {
    pub id: String,
    /// `personal`, or a kind a plugin contributes (`<plugin>.<name>`).
    pub kind: String,
    pub name: String,
    /// The URL segment: `/<slug>/<plugin>/<view>`. `~` is always the caller's own personal space.
    pub slug: String,
    pub personal: bool,
    /// The caller's level on the space.
    pub level: Option<crate::methods::Grant>,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SpaceList {
    pub spaces: Vec<SpaceInfo>,
}

/// `spaces.get`, `spaces.enter`, `spaces.members`: a space by id or slug; `~` is the personal space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SpaceRef {
    pub space: String,
}

/// `spaces.create`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SpaceCreate {
    /// A kind from some plugin's `spaceKinds`.
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub slug: Option<String>,
}

/// `spaces.rename`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SpaceRename {
    pub space: String,
    pub name: String,
}

/// A space kind with the plugin that contributes it (`spaces.kinds`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SpaceKindInfo {
    pub id: String,
    pub plugin: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SpaceKindList {
    pub kinds: Vec<SpaceKindInfo>,
}

// ── entities and references ──────────────────────────────────────────────────────────────────────

/// An entry of the entity index: enough to render a chip without asking the owning plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct EntityInfo {
    pub uri: String,
    pub scheme: String,
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub icon: Option<String>,
    pub space_id: Option<String>,
    pub parent: Option<String>,
    pub plugin: Option<String>,
    pub private: bool,
    pub updated_at: String,
    /// Set when the entity is in the trash.
    pub deleted_at: Option<String>,
}

/// `entities.resolve`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EntityRef {
    pub uri: String,
}

/// `entities.resolveMany`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EntityRefs {
    pub uris: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EntityList {
    pub entities: Vec<EntityInfo>,
}

/// `entities.search`: the mention picker and global search. Only what the caller may view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EntitySearch {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub schemes: Option<Vec<String>>,
    /// Only entities in this space (id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub space: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub limit: Option<i64>,
}

/// `entities.alias` (plugins): `uri`, in a scheme of the plugin, now names the entity `target`. Resolving it
/// gives the target, access to it is the target's, and references to it are the target's backlinks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EntityAlias {
    pub uri: String,
    pub target: String,
}

/// `entities.register` (plugins): an entity that is not a collection row, in a scheme the plugin
/// declares in `entityTypes`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct EntityRegister {
    pub uri: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub subtitle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub icon: Option<String>,
    /// More text to search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub search_text: Option<String>,
    /// A space id; the entity inherits the space's grants unless it has a parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub space: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub parent: Option<String>,
    #[serde(default)]
    pub private: bool,
    /// Outgoing references, for backlinks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub refs: Option<Vec<String>>,
}

/// One inbound reference (`data.refsTo`): where `uri` is referenced or mentioned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Backlink {
    /// The referencing entity, or `row://<plugin>/<collection>/<id>` for rows that are not entities.
    pub source: String,
    /// The field the reference is in.
    pub field: String,
    pub plugin: Option<String>,
    pub collection: Option<String>,
    pub row_id: Option<String>,
    pub title: Option<String>,
    pub icon: Option<String>,
    /// The source's subtitle in the entity index (its state, say).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub subtitle: Option<String>,
    /// When the reference was first made (RFC 3339). It stays while the source keeps referencing.
    #[serde(default)]
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BacklinkList {
    pub backlinks: Vec<Backlink>,
}

/// One outgoing reference (`data.refsFrom`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Reference {
    pub field: String,
    pub target: String,
    /// The target no longer exists.
    pub dangling: bool,
    pub title: Option<String>,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ReferenceList {
    pub references: Vec<Reference>,
}

/// `data.refsTo`, `data.refsFrom`: an entity by its URI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RefsOf {
    pub uri: String,
}

// ── collections ──────────────────────────────────────────────────────────────────────────────────

/// A structured query (the filter is in `crates/rookery-filter/FILTER.md`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DataQuery {
    #[serde(default, rename = "where", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "ts",
        ts(optional, rename = "where", type = "Record<string, unknown>")
    )]
    pub filter: Option<Value>,
    /// `["field", "desc"]` or `"field"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "ts",
        ts(optional, type = "Array<string | [string, \"asc\" | \"desc\"]>")
    )]
    pub order_by: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub limit: Option<i64>,
    /// The previous page's `next`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub after: Option<String>,
    /// Full-text search over the fields marked `search`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub search: Option<String>,
    /// Query these spaces (ids) instead of the current one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub spaces: Option<Vec<String>>,
    /// Only rows in the trash.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(optional, type = "boolean"))]
    pub trashed: bool,
    /// The declared fields to return; the system fields are always there. Without it, every field but `bytes`
    /// and `vector` ones, which are returned only when named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub select: Option<Vec<String>>,
    /// Vector search: the rows nearest `to` by the field's metric, each with `_distance`. Replaces `orderBy`,
    /// `after` and `search`; `limit` is at most 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub nearest: Option<NearestQuery>,
    /// Up to five `ref` or `refs` fields whose rows come back beside the page, in `included`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub include: Option<Vec<String>>,
}

/// `nearest`: the rows closest to a query vector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct NearestQuery {
    /// A `vector` field.
    pub field: String,
    /// The query vector; its length is the field's dimensions.
    pub to: Vec<f32>,
    /// Only rows at most this far away, in the field's metric (cosine and l2 distance; for `ip` the negative
    /// inner product).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub max_distance: Option<f64>,
}

/// The collection a call is about. Plugins name only their own collections (`plugin` is ignored);
/// clients name the plugin. `space` (an id) overrides the current space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub plugin: Option<String>,
    pub collection: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub space: Option<String>,
}

/// `data.find`, `data.count`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataFind {
    #[serde(flatten)]
    pub target: DataTarget,
    #[serde(default)]
    pub query: DataQuery,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataPage {
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>[]"))]
    pub rows: Vec<Value>,
    /// The cursor of the next page, when there is one.
    pub next: Option<String>,
    /// With `include`: the rows the page's `ref` and `refs` fields point to that the caller may read, by the
    /// reference's value (an entity URI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "ts",
        ts(optional, type = "Record<string, Record<string, unknown>>")
    )]
    pub included: Option<serde_json::Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataCount {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub count: i64,
}

/// `data.search`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataSearch {
    #[serde(flatten)]
    pub target: DataTarget,
    pub text: String,
    #[serde(default, rename = "where", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "ts",
        ts(optional, rename = "where", type = "Record<string, unknown>")
    )]
    pub filter: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub limit: Option<i64>,
}

/// `data.get`, `data.delete`, `data.restore`, `data.history`: one row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataRowRef {
    #[serde(flatten)]
    pub target: DataTarget,
    pub id: String,
    /// `data.get`: look in the trash.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(optional, type = "boolean"))]
    pub trashed: bool,
    /// `data.history`: at most this many entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub limit: Option<i64>,
    /// Makes a retried write answer as the first one did, instead of writing again, for a day.
    #[serde(
        default,
        rename = "idempotencyKey",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(feature = "ts", ts(optional, rename = "idempotencyKey"))]
    pub idempotency_key: Option<String>,
}

/// A row, or `null`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataRow {
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown> | null"))]
    pub row: Option<Value>,
}

/// `data.insert`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataInsert {
    #[serde(flatten)]
    pub target: DataTarget,
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
    pub values: Value,
    /// Makes a retried write answer as the first one did, instead of writing again, for a day.
    #[serde(
        default,
        rename = "idempotencyKey",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(feature = "ts", ts(optional, rename = "idempotencyKey"))]
    pub idempotency_key: Option<String>,
}

/// `data.update`. With `rev`, fails with `CONFLICT` unless the row is still at that revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub ops: Option<std::collections::BTreeMap<String, FieldOp>>,
    #[serde(flatten)]
    pub target: DataTarget,
    pub id: String,
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
    pub patch: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub rev: Option<i64>,
    /// Makes a retried write answer as the first one did, instead of writing again, for a day.
    #[serde(
        default,
        rename = "idempotencyKey",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(feature = "ts", ts(optional, rename = "idempotencyKey"))]
    pub idempotency_key: Option<String>,
}

/// Atomic field operations, validated against the manifest's field types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum FieldOp {
    Inc(#[cfg_attr(feature = "ts", ts(type = "number"))] serde_json::Number),
    Add(Vec<String>),
    Remove(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct LeaseCondition {
    pub key: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub token: i64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct BatchCondition {
    pub lease: LeaseCondition,
}

/// `data.upsert` on a declared unique key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataUpsert {
    #[serde(flatten)]
    pub target: DataTarget,
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
    pub values: Value,
    pub on: Vec<String>,
    /// Makes a retried write answer as the first one did, instead of writing again, for a day.
    #[serde(
        default,
        rename = "idempotencyKey",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(feature = "ts", ts(optional, rename = "idempotencyKey"))]
    pub idempotency_key: Option<String>,
}

/// One operation of `data.batch`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum BatchOp {
    Upsert {
        collection: String,
        #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
        values: Value,
        on: Vec<String>,
    },
    Publish {
        topic: String,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        payload: Value,
    },
    Enqueue {
        job: String,
        #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
        params: Value,
    },
    Insert {
        collection: String,
        #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
        values: Value,
    },
    Update {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        ops: Option<std::collections::BTreeMap<String, FieldOp>>,
        collection: String,
        id: String,
        #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
        patch: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        rev: Option<i64>,
    },
    Delete {
        collection: String,
        id: String,
    },
    Restore {
        collection: String,
        id: String,
    },
}

/// `data.batch`: one transaction over one plugin's collections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataBatch {
    #[serde(default, rename = "if", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, rename = "if"))]
    pub condition: Option<BatchCondition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub plugin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub space: Option<String>,
    pub ops: Vec<BatchOp>,
    /// Makes a retried write answer as the first one did, instead of writing again, for a day.
    #[serde(
        default,
        rename = "idempotencyKey",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(feature = "ts", ts(optional, rename = "idempotencyKey"))]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataBatchResult {
    /// The row after each operation, `null` after a hard delete.
    #[cfg_attr(feature = "ts", ts(type = "Array<Record<string, unknown> | null>"))]
    pub rows: Vec<Option<Value>>,
}

/// One field-level change of a row (`data.history`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    /// `insert`, `update`, `delete` or `restore`.
    pub op: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub rev: i64,
    /// Who acted: `user:<id>`, `agent:<id>`, ...
    pub principal: String,
    /// The user an agent or service acted for.
    pub on_behalf_of: Option<String>,
    pub at: String,
    /// `{ field: [from, to] }`; bytes use `{ changed: true }` without content.
    #[cfg_attr(
        feature = "ts",
        ts(type = "Record<string, [unknown, unknown] | { changed: true }>")
    )]
    pub changes: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HistoryList {
    pub entries: Vec<HistoryEntry>,
}

// ── live queries and the change feed ─────────────────────────────────────────────────────────────

/// `data.live`: runs the query and pushes `data.changed` for every later change to the collection in
/// the query's spaces that the caller may read. With `after` (the `seq` of an earlier subscription), it
/// resumes instead: no rows, then every change after that seq, or `reset` if the log no longer has them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct LiveQuery {
    #[serde(flatten)]
    pub target: DataTarget,
    #[serde(default)]
    pub query: DataQuery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub after: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct LiveStarted {
    pub subscription: String,
    /// The first page, unless resuming.
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>[] | null"))]
    pub rows: Option<Vec<Value>>,
    pub next: Option<String>,
    /// With `include`: see `DataPage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "ts",
        ts(optional, type = "Record<string, Record<string, unknown>>")
    )]
    pub included: Option<serde_json::Map<String, Value>>,
    /// Changes after this seq are delivered.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub seq: i64,
}

/// `data.unlive`, `data.unsubscribe`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SubscriptionRef {
    pub subscription: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum ChangeOp {
    Insert,
    Update,
    /// To the trash (`row.deletedAt` set) or for good.
    Delete,
    Restore,
}

/// One committed change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Change {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub seq: i64,
    pub collection: String,
    pub id: String,
    pub op: ChangeOp,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub rev: i64,
    pub space_id: Option<String>,
    pub at: String,
    /// The row after the change. `null` when it is gone, or (in `data.changed` to clients) when the
    /// subscriber may not read it.
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown> | null"))]
    pub row: Option<Value>,
}

/// Notification `data.changed`, to clients for `data.live` and to host halves for `data.subscribe`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataChanged {
    pub subscription: String,
    pub changes: Vec<Change>,
    /// The subscriber missed changes the log no longer has: refetch, then go on from `seq`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub reset: Option<i64>,
    /// The subscription ended, for the reason in `error` (such as `FORBIDDEN` once the subscriber lost
    /// view on the space it follows). Nothing follows it, and it carries no changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub ended: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub error: Option<crate::error::ErrorBody>,
}

/// Notification `data.changed` to the extension host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PluginDataChanged {
    pub plugin: String,
    #[serde(flatten)]
    pub changed: DataChanged,
}

/// `data.subscribe` (host halves): every change to the plugin's collections, or to these ones, after
/// `after` (default: from now).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FeedSubscribe {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub collections: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub after: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FeedSubscription {
    pub subscription: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub seq: i64,
}

// ── plugin migrations ────────────────────────────────────────────────────────────────────────────

/// `plugins.transform` (kernel to host): run the transform step `name` of a migration. Its code is
/// `migrations/<name>.ts`; it reads and writes rows with `data.tx*` calls carrying `migration`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Transform {
    pub plugin: String,
    pub dir: String,
    pub name: String,
    pub migration: String,
}

/// `data.txRows`: rows of a collection in id order, in every scope and including the trash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TxRows {
    pub migration: String,
    pub collection: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub limit: Option<i64>,
}

/// `data.txLegacy`: rows of a table a kernel release retired, as JSON, in a stable order after a cursor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TxLegacy {
    pub migration: String,
    pub table: String,
    /// The `next` of the page before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub limit: Option<i64>,
}

/// `data.txInsert`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TxInsert {
    pub migration: String,
    pub collection: String,
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
    pub values: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub space: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub user: Option<String>,
}

/// `data.txUpdate`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TxUpdate {
    pub migration: String,
    pub collection: String,
    pub id: String,
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
    pub patch: Value,
}

/// `data.txDelete`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TxDelete {
    pub migration: String,
    pub collection: String,
    pub id: String,
}

// ── aggregates, scans, imports and logs ──────────────────────────────────────────────────────────

/// `data.aggregate`: grouped counts, sums and extremes over the rows the caller may read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DataAggregate {
    #[serde(flatten)]
    pub target: DataTarget,
    #[serde(default, rename = "where", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "ts",
        ts(optional, rename = "where", type = "Record<string, unknown>")
    )]
    pub filter: Option<Value>,
    /// Up to three keys; a date or datetime field can be bucketed by `day`, `week` (Monday) or `month` in UTC.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<Vec<GroupKey>>"))]
    pub group_by: Vec<GroupKey>,
    /// Up to twenty, by name; each has exactly one function.
    pub measures: std::collections::BTreeMap<String, AggregateMeasure>,
    /// Group keys and measures by name, `"n"` or `["n", "desc"]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "ts",
        ts(optional, type = "Array<string | [string, \"asc\" | \"desc\"]>")
    )]
    pub order_by: Option<Value>,
    /// Keep this many groups (at most 1,000). Without it, more than 1,000 groups is an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub spaces: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(untagged)]
pub enum GroupKey {
    Field(String),
    Bucketed { field: String, by: GroupBucket },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum GroupBucket {
    Day,
    Week,
    Month,
}

/// One aggregate function and its field (`count` takes `true`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AggregateMeasure {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub count: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub count_distinct: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub sum: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub avg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub min: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub max: Option<String>,
}

/// The groups: one object each, with the group keys and the measures by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataAggregateResult {
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>[]"))]
    pub rows: Vec<Value>,
}

/// `data.import`: up to 500 rows, one transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataImport {
    #[serde(flatten)]
    pub target: DataTarget,
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>[]"))]
    pub rows: Vec<Value>,
    /// `insert` (the default) or `upsert` on `on`.
    #[serde(default)]
    pub mode: ImportMode,
    /// `upsert`: the unique key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<Vec<String>>"))]
    pub on: Vec<String>,
    /// Makes a retried write answer as the first one did, instead of writing again, for a day.
    #[serde(
        default,
        rename = "idempotencyKey",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(feature = "ts", ts(optional, rename = "idempotencyKey"))]
    pub idempotency_key: Option<String>,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum ImportMode {
    #[default]
    Insert,
    Upsert,
}

/// A row `data.import` did not write, by its position in `rows`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ImportFailure {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub index: u32,
    /// The Rookery error code: `INVALID`, `CONFLICT`, `NOT_FOUND`.
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "Record<string, unknown>"))]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataImportResult {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub inserted: u64,
    /// Rows `upsert` found and changed.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub updated: u64,
    pub failed: Vec<ImportFailure>,
}

/// `data.tail`: the rows of a log collection after a `seq`, oldest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DataTail {
    #[serde(flatten)]
    pub target: DataTarget,
    /// Rows with a larger `seq`; 0 reads from the start.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub from_seq: i64,
    #[serde(default, rename = "where", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "ts",
        ts(optional, rename = "where", type = "Record<string, unknown>")
    )]
    pub filter: Option<Value>,
    /// At most this many (default and most 1,000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub select: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub spaces: Option<Vec<String>>,
}

/// The rows, and the `seq` to continue from: the last row's, or `fromSeq` when there are none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DataTailResult {
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>[]"))]
    pub rows: Vec<Value>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub seq: i64,
}

// ── entity text ──────────────────────────────────────────────────────────────────────────────────────

/// The longest text of one entity `entities.text` returns, whatever `max_chars` says.
pub const ENTITY_TEXT_MAX_CHARS: u32 = 200_000;
/// What `entities.text` cuts an entity's text to when the caller does not say.
pub const ENTITY_TEXT_DEFAULT_CHARS: u32 = 20_000;
/// The most entities one `entities.text` call takes.
pub const ENTITY_TEXT_MAX_URIS: usize = 50;

/// How a text is written.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum EntityTextFormat {
    /// Text with no markup.
    #[default]
    Plain,
    /// Markdown: headings and lists the reader may render or read as they are.
    Markdown,
}

/// `entities.text` (rookery.v1's `Entities.Text`): entities as text, to read, search or hand to a model. Each is
/// turned into text by the plugin that owns it (the handler `entities.text:<scheme>`, which `rookery.entities.text`
/// registers or an entity type's `textHandler` binds to a service method), or, for a row of an entity collection,
/// generated from its fields, and only if the caller may read it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct EntityTextQuery {
    pub uris: Vec<String>,
    /// Per entity; longer text is cut (`truncated`). Default 20000 characters, at most 200000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub max_chars: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EntityText {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub title: Option<String>,
    pub text: String,
    /// How `text` is written.
    pub format: EntityTextFormat,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EntityTextList {
    pub texts: Vec<EntityText>,
    /// URIs that do not exist, that the caller may not read, or whose owner gives no text.
    pub missing: Vec<String>,
}

/// The params of `entities.text:<scheme>` in the owning plugin. The kernel has checked access.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EntityTextCall {
    pub uri: String,
    pub scheme: String,
    pub id: String,
}

/// What the owner of an entity type answers to `entities.text:<scheme>`, or null for no text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EntityTextResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub title: Option<String>,
    pub text: String,
    /// Plain when missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub format: Option<EntityTextFormat>,
}
