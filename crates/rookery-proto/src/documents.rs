//! Collaborative documents (`documents.*`): a Yjs document per id, an entity `document://<id>` attached to a
//! parent entity it inherits permissions from (`view` reads, `use` edits), synced with the y-protocols
//! messages over a stream channel, with version history and server-side edits for host halves.
//!
//! The content is ProseMirror JSON in the shared editor's schema (see `@crow/ui`'s `richTextSchema`): the
//! Yjs document holds it in the XML fragment `prosemirror`, as `y-prosemirror` writes it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `documents.create`. The caller needs `use` on `parent`; without a parent the document belongs to the
/// caller alone (they own it, and share it like any entity).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DocumentCreate {
    /// The entity the document is attached to (`note://…`, `issue://…`). Permissions inherit from it, and
    /// mentions in the document are references from it (see `DocumentInfo.source`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub parent: Option<String>,
    /// The space it belongs to (an id). Defaults to the parent's space, then the caller's current space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub space: Option<String>,
    /// The entity title until the document has text of its own (its first block names it after that).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub title: Option<String>,
    /// Initial content as ProseMirror JSON (a `doc` node, or a list of block nodes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
    pub content: Option<Value>,
    /// Initial content as Markdown, when `content` is not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub markdown: Option<String>,
}

/// A document as `documents.get`, `create` and `list` describe it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DocumentInfo {
    pub id: String,
    /// `document://<id>`.
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub parent: Option<String>,
    /// The URI mentions in the document are indexed from: the parent entity, or the document itself when
    /// it has no parent or its parent is a space or a plugin.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub space_id: Option<String>,
    /// The plugin that created it, if a plugin did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub plugin: Option<String>,
    /// The entity index title: the first block's text, or the title given at creation while it is empty.
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub created_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// The caller's level: `view` (read only), `use` (edit) or `manage`.
    pub level: String,
}

/// `documents.get`, `documents.delete`, `documents.history`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DocumentRef {
    pub id: String,
}

/// `documents.list`: the documents attached to an entity that the caller may view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DocumentListQuery {
    pub parent: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DocumentList {
    pub documents: Vec<DocumentInfo>,
}

/// `documents.open` (clients on `/ws`): opens a stream carrying y-protocols messages, each framed as a
/// varuint length followed by the message (`[0, step, …]` sync, `[1, …]` awareness). The kernel starts
/// with its sync step 1 and the awareness of everyone present; the client answers with its own step 1,
/// which is how edits made offline reach the kernel on reconnect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DocumentOpen {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DocumentOpened {
    pub stream: u32,
    pub document: DocumentInfo,
    /// The caller may only view: updates it sends are refused with an auth message and dropped.
    pub read_only: bool,
}

/// What `documents.read` returns.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum DocumentFormat {
    /// ProseMirror JSON.
    #[default]
    Json,
    Markdown,
    /// Plain text, as indexed for search.
    Text,
}

/// `documents.read`: the current content, or a version's (`version`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DocumentRead {
    pub id: String,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub format: Option<DocumentFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub version: Option<String>,
}

/// `documents.convert`: Markdown to ProseMirror JSON in the shared editor's schema, or JSON to Markdown, with
/// the same converters documents use (`markdown` in, `content` out, or the other way). No document is needed:
/// for rich-text fields that sync with Markdown elsewhere (a comment from GitHub).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DocumentConvert {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub markdown: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
    pub content: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DocumentConverted {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub markdown: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
    pub content: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DocumentContent {
    pub id: String,
    pub title: String,
    /// ProseMirror JSON (`format: "json"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
    pub content: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub markdown: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub text: Option<String>,
    /// How many top-level blocks the document has; block indexes in edits count these.
    pub blocks: u32,
}

/// Who a server-side edit shows as to the people in the document (their awareness entry).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DocumentAuthor {
    pub name: String,
    /// A CSS colour for the author's presence; one is picked from the name when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub color: Option<String>,
}

/// One server-side edit. Blocks are the document's top-level nodes, counted from 0. Content is ProseMirror
/// JSON (a node, a list of nodes, or a `doc`) or Markdown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum DocumentEditOp {
    /// Inserts blocks before block `at` (at the end when `at` is absent or past the end).
    #[serde(rename_all = "camelCase")]
    Insert {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        at: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
        content: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        markdown: Option<String>,
    },
    /// Replaces blocks `from` up to (not including) `to` with the content.
    #[serde(rename_all = "camelCase")]
    ReplaceBlocks {
        from: u32,
        to: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
        content: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        markdown: Option<String>,
    },
    /// Deletes blocks `from` up to (not including) `to`.
    DeleteBlocks { from: u32, to: u32 },
    /// Replaces characters `from`..`to` of a text block (a paragraph, heading or code block; offsets in
    /// UTF-16 code units, an inline node such as a mention counts as one) with plain text, which takes the
    /// marks of the character before it. Concurrent typing elsewhere in the block is kept.
    ReplaceText {
        block: u32,
        from: u32,
        to: u32,
        text: String,
    },
    /// Appends Markdown at the end of the document.
    AppendMarkdown { markdown: String },
    /// Replaces the whole document.
    #[serde(rename_all = "camelCase")]
    SetContent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
        content: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        markdown: Option<String>,
    },
}

/// `documents.edit`: applies the operations in order, in one Yjs transaction, as a participant of its own
/// (the caller's principal), so the people in the document see the author's presence and the edit merges
/// with theirs. Needs `use`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DocumentEdit {
    pub id: String,
    pub ops: Vec<DocumentEditOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub author: Option<DocumentAuthor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DocumentEdited {
    /// Top-level blocks after the edit.
    pub blocks: u32,
}

/// A saved state of a document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DocumentVersion {
    pub id: String,
    pub document_id: String,
    /// `named` (someone saved it), `auto` (periodic, and when an editing session ends) or `restore` (the
    /// state just before a restore).
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub name: Option<String>,
    /// The principal that made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub created_by: Option<String>,
    pub created_at: String,
    /// Bytes of the stored state.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub size: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DocumentVersions {
    pub versions: Vec<DocumentVersion>,
}

/// `documents.saveVersion`: a named version of the current state. Needs `use`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DocumentSaveVersion {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub name: Option<String>,
}

/// `documents.restore`: makes the document what it was in a version. The current state is saved as a
/// `restore` version first; the change reaches everyone in the document as an ordinary edit. Needs `use`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DocumentRestore {
    pub id: String,
    pub version: String,
}
