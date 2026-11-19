//! Collection definitions: the JSON a plugin declares under `contributes.collections`.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::caller::Level;
use crate::error::{Error, Result};

/// One collection, as declared in the manifest. See `docs/data.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionDef {
    pub name: String,
    #[serde(default)]
    pub scope: Scope,
    /// Rows are entities: they get a URI, appear in the entity index, and can be referenced,
    /// mentioned and shared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<EntityDef>,
    #[serde(default)]
    pub access: AccessDef,
    /// Store field-level changes of every write.
    #[serde(default)]
    pub history: bool,
    /// Deletes are soft; rows can be restored.
    #[serde(default)]
    pub trash: bool,
    pub fields: Vec<FieldDef>,
    /// Extra (composite) indexes, each a list of field names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<Vec<String>>,
    /// Composite unique constraints, each a list of field names. Single fields can also set
    /// `unique` on the field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unique: Vec<Vec<String>>,
    /// `regular` (the default) or `log`: append-only, partitioned by time, with no change log entries.
    #[serde(default, skip_serializing_if = "CollectionKind::is_regular")]
    pub kind: CollectionKind,
    /// Log collections: partitions older than this are dropped whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<Retention>,
    /// Log collections: the size of a partition. Default `day`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<Partition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollectionKind {
    #[default]
    Regular,
    Log,
}

impl CollectionKind {
    pub fn is_regular(&self) -> bool {
        *self == CollectionKind::Regular
    }
}

/// How long a log collection keeps rows: whole partitions older than `days` are dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retention {
    pub days: u32,
}

/// The time span of one partition of a log collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Partition {
    Day,
    Month,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Global,
    #[default]
    Space,
    User,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Space => "space",
            Scope::User => "user",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityDef {
    /// The URI scheme of rows: `<scheme>://<row id>`. Unique across plugins.
    pub scheme: String,
    /// The field holding the title.
    pub title: String,
    /// A field holding a subtitle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// A field holding an icon name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// A `ref` field naming the parent entity. Without one, the parent is the row's space
    /// (space scope), user (user scope) or `plugin://<plugin>` (global scope).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// A `boolean` field; rows with it set are private and do not inherit grants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private: Option<String>,
    /// The text `entities.text` makes of a row: `false` for none, `{ "fields": [..] }` for those fields
    /// alone. Without it, every field but the title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<serde_json::Value>,
}

/// The permission level reading and writing need, on the row (entity collections) or on the
/// row's scope container (other collections).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessDef {
    #[serde(default = "level_view")]
    pub read: Level,
    #[serde(default = "level_use")]
    pub write: Level,
    /// Deleting; defaults to `write`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<Level>,
    /// Only admins read and write it, whatever a grant says: a caller that acts for a user who is not an admin
    /// is refused. A caller without a user (the plugin's own service, a job) is not: it acts as the plugin.
    #[serde(default, skip_serializing_if = "is_false")]
    pub admin: bool,
}

fn level_view() -> Level {
    Level::View
}
fn level_use() -> Level {
    Level::Use
}

impl Default for AccessDef {
    fn default() -> Self {
        AccessDef {
            read: Level::View,
            write: Level::Use,
            delete: None,
            admin: false,
        }
    }
}

impl AccessDef {
    pub fn delete_level(&self) -> Level {
        self.delete.unwrap_or(self.write)
    }

    /// Refuses a caller who is not an admin when the collection is for admins.
    pub(crate) fn check_admin(
        &self,
        plugin: &str,
        name: &str,
        caller: &crate::caller::Caller,
    ) -> Result<()> {
        if self.admin && !caller.admin_on(plugin) {
            return Err(Error::forbidden(format!(
                "the collection {name} is for admins"
            )));
        }
        Ok(())
    }
}

/// The semantic colour tone of an enum value in badges, chips and board columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EnumTone {
    Neutral,
    Accent,
    Positive,
    Negative,
}

/// One option of an `enum` field: either a plain value string or `{ value, title?, tone? }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EnumValue {
    Name(String),
    Detail {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tone: Option<EnumTone>,
    },
}

impl EnumValue {
    pub fn value(&self) -> &str {
        match self {
            EnumValue::Name(v) => v,
            EnumValue::Detail { value, .. } => value,
        }
    }
}

/// Presentation format of a field value in generic views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldFormat {
    Currency,
    Percent,
    Duration,
    Progress,
    Url,
    Email,
}

impl FieldFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldFormat::Currency => "currency",
            FieldFormat::Percent => "percent",
            FieldFormat::Duration => "duration",
            FieldFormat::Progress => "progress",
            FieldFormat::Url => "url",
            FieldFormat::Email => "email",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldDef {
    pub name: String,
    #[serde(flatten)]
    pub ty: FieldType,
    /// Allowed enum transitions: `from -> to[]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transitions: Option<BTreeMap<String, Vec<String>>>,
    /// Included in full-text search.
    #[serde(default, skip_serializing_if = "is_false")]
    pub search: bool,
    /// Boolean for ordinary fields, `hnsw` or `none` for vectors; a vector without it is indexed by
    /// `hnsw`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<FieldIndex>,
    /// Gets a unique index (within the row's scope).
    #[serde(default, skip_serializing_if = "is_false")]
    pub unique: bool,
    /// With `unique` on a text field: compare case-insensitively.
    #[serde(default, skip_serializing_if = "is_false")]
    pub case_insensitive: bool,
    /// Inserts must set it and updates cannot clear it. Checked by the store, not the database,
    /// so making a field required later does not touch existing rows.
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    /// Minimum numeric value (`integer`, `number`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Maximum numeric value (`integer`, `number`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Minimum character length (`text`, `longtext`, `path`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u32>,
    /// Maximum character length (`text`, `longtext`, `path`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u32>,
    /// Regex pattern (`text`, `longtext`, `path`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Used on insert when the field is not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// Display title for generic views.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// For the UI; ignored by the store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<FieldFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_when: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editable_when: Option<Value>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Vector distance metric (cosine by default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VectorMetric {
    #[default]
    Cosine,
    L2,
    Ip,
}

/// Vector indexes share the manifest's `index` attribute with ordinary boolean indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FieldIndex {
    Boolean(bool),
    Vector(VectorIndex),
}
impl Default for FieldIndex {
    fn default() -> Self {
        Self::Boolean(false)
    }
}
impl FieldIndex {
    pub fn is_false(&self) -> bool {
        *self == Self::Boolean(false)
    }
    pub fn enabled(&self) -> bool {
        *self == Self::Boolean(true)
    }
}

impl FieldDef {
    /// Whether an ordinary field has a `true` index.
    pub fn indexed(&self) -> bool {
        self.index.is_some_and(|i| i.enabled())
    }

    /// The index of a vector field: `hnsw` unless it says `none`, and `None` for another field or a
    /// value [`CollectionDef::validate`] refuses.
    pub fn vector_index(&self) -> Option<VectorIndex> {
        match self.index {
            Some(FieldIndex::Vector(v)) => Some(v),
            None if matches!(self.ty, FieldType::Vector { .. }) => Some(VectorIndex::Hnsw),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VectorIndex {
    Hnsw,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum FieldType {
    Bytes {
        #[serde(default = "default_max_bytes", rename = "maxBytes")]
        max_bytes: u32,
    },
    Blob,
    Vector {
        dimensions: u32,
        #[serde(default)]
        metric: VectorMetric,
    },
    Text,
    Longtext,
    /// ProseMirror JSON. Plain text is extracted for search and mentions for references.
    Richtext,
    Integer,
    Number,
    Boolean,
    /// `YYYY-MM-DD`.
    Date,
    /// RFC 3339.
    Datetime,
    Enum {
        values: Vec<EnumValue>,
    },
    Json,
    /// An entity URI. With `to`, a bare id is accepted and stored as `<to>://<id>`.
    Ref {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
    },
    /// A list of entity URIs.
    Refs {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
    },
    Path,
    /// A counter assigned by the store on insert (1, 2, 3, ...), per space, user or globally.
    Sequence {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        per: Option<Scope>,
    },
}

pub fn default_max_bytes() -> u32 {
    1024 * 1024
}

impl FieldType {
    pub fn name(&self) -> &'static str {
        match self {
            FieldType::Bytes { .. } => "bytes",
            FieldType::Blob => "blob",
            FieldType::Vector { .. } => "vector",
            FieldType::Text => "text",
            FieldType::Longtext => "longtext",
            FieldType::Richtext => "richtext",
            FieldType::Integer => "integer",
            FieldType::Number => "number",
            FieldType::Boolean => "boolean",
            FieldType::Date => "date",
            FieldType::Datetime => "datetime",
            FieldType::Enum { .. } => "enum",
            FieldType::Json => "json",
            FieldType::Ref { .. } => "ref",
            FieldType::Refs { .. } => "refs",
            FieldType::Path => "path",
            FieldType::Sequence { .. } => "sequence",
        }
    }

    /// The Postgres column type.
    pub(crate) fn sql_type(&self) -> String {
        if let Self::Vector { dimensions, .. } = self {
            return format!("vector({dimensions})");
        }
        self.sql_type_inner().to_string()
    }

    fn sql_type_inner(&self) -> &'static str {
        match self {
            FieldType::Bytes { .. } => "bytea",
            FieldType::Vector { .. } => "vector",
            FieldType::Blob
            | FieldType::Text
            | FieldType::Longtext
            | FieldType::Enum { .. }
            | FieldType::Ref { .. }
            | FieldType::Path => "text",
            FieldType::Richtext | FieldType::Json => "jsonb",
            FieldType::Integer | FieldType::Sequence { .. } => "bigint",
            FieldType::Number => "double precision",
            FieldType::Boolean => "boolean",
            FieldType::Date => "date",
            FieldType::Datetime => "timestamptz",
            FieldType::Refs { .. } => "text[]",
        }
    }

    pub(crate) fn is_texty(&self) -> bool {
        matches!(
            self,
            FieldType::Text
                | FieldType::Longtext
                | FieldType::Enum { .. }
                | FieldType::Path
                | FieldType::Richtext
        )
    }
}

/// Names a field cannot have: the system columns in both spellings, the filter operators, and
/// anything starting with `_` (reserved for the store).
pub(crate) const RESERVED_NAMES: &[&str] = &[
    "id",
    "space_id",
    "spaceId",
    "user_id",
    "userId",
    "created_at",
    "createdAt",
    "updated_at",
    "updatedAt",
    "created_by",
    "createdBy",
    "updated_by",
    "updatedBy",
    "rev",
    "deleted_at",
    "deletedAt",
    "and",
    "or",
    "not",
];

pub(crate) fn valid_ident(s: &str, max: usize) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && s.len() <= max
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Plugin ids: lower-case letters, digits and dashes, starting with a letter.
pub fn validate_plugin_id(id: &str) -> Result<()> {
    let mut chars = id.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && id.len() <= 40
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!("bad plugin id {id:?}")))
    }
}

/// The Postgres schema of a plugin's collections: `p_<id with - replaced by _>`, and `rookery` for
/// Rookery's own.
pub fn plugin_schema(plugin: &str) -> String {
    if plugin == super::system::PLUGIN {
        return plugin.to_string();
    }
    format!("p_{}", plugin.replace('-', "_"))
}

impl FieldDef {
    /// A field of a type with nothing else set.
    pub fn named(name: &str, ty: FieldType) -> FieldDef {
        let mut v = serde_json::to_value(&ty).expect("a field type serializes");
        v["name"] = Value::String(name.into());
        serde_json::from_value(v).expect("a field of a name and a type")
    }
}

impl CollectionDef {
    /// A collection of a scope and fields, with nothing else set.
    pub fn named(name: &str, scope: Scope, fields: Vec<FieldDef>) -> CollectionDef {
        CollectionDef {
            name: name.into(),
            scope,
            entity: None,
            access: AccessDef::default(),
            history: false,
            trash: false,
            fields,
            indexes: Vec::new(),
            unique: Vec::new(),
            kind: CollectionKind::Regular,
            retention: None,
            partition: None,
        }
    }

    pub fn field(&self, name: &str) -> Option<&FieldDef> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// Checks names, references between options and fields, and type-specific options.
    pub fn validate(&self) -> Result<()> {
        let bad = |msg: String| Err(Error::invalid(format!("collection {}: {msg}", self.name)));
        if !valid_ident(&self.name, 48) {
            return bad(
                "collection names are letters, digits and _, starting with a letter, at most 48 long".into(),
            );
        }
        self.validate_kind()?;
        if self.fields.len() > 200 {
            return bad("at most 200 fields".into());
        }
        let mut seen = HashSet::new();
        for f in &self.fields {
            if !valid_ident(&f.name, 63) || RESERVED_NAMES.contains(&f.name.as_str()) {
                return bad(format!("{:?} is not a valid field name", f.name));
            }
            if !seen.insert(f.name.as_str()) {
                return bad(format!("field {} is declared twice", f.name));
            }
            match (&f.ty, f.index) {
                (FieldType::Vector { dimensions, .. }, index) => {
                    let max = match index {
                        None | Some(FieldIndex::Vector(VectorIndex::Hnsw)) => 2000,
                        Some(FieldIndex::Vector(VectorIndex::None)) => 16000,
                        Some(FieldIndex::Boolean(_)) => {
                            return bad(format!(
                                "field {}: vector index must be hnsw or none",
                                f.name
                            ));
                        }
                    };
                    if *dimensions == 0 || *dimensions > max {
                        return bad(format!(
                            "field {}: vector dimensions must be 1..={max}",
                            f.name
                        ));
                    }
                }
                (_, Some(FieldIndex::Vector(_))) => {
                    return bad(format!(
                        "field {}: string index is only valid for vectors",
                        f.name
                    ));
                }
                _ => {}
            }
            if matches!(f.ty, FieldType::Bytes { .. }) && f.indexed() {
                return bad(format!("field {}: bytes cannot be indexed", f.name));
            }
            match &f.ty {
                FieldType::Enum { values } => {
                    if values.is_empty() {
                        return bad(format!("enum field {} has no values", f.name));
                    }
                    let mut seen_vals = HashSet::new();
                    for v in values {
                        let s = v.value();
                        if s.is_empty() {
                            return bad(format!("enum field {} has an empty value", f.name));
                        }
                        if !seen_vals.insert(s) {
                            return bad(format!("enum field {} repeats value {s:?}", f.name));
                        }
                    }
                    if let Some(transitions) = &f.transitions {
                        for (from, targets) in transitions {
                            if !seen_vals.contains(from.as_str()) {
                                return bad(format!(
                                    "field {}: transition references unknown enum value {from:?}",
                                    f.name
                                ));
                            }
                            for to in targets {
                                if !seen_vals.contains(to.as_str()) {
                                    return bad(format!(
                                        "field {}: transition references unknown enum value {to:?}",
                                        f.name
                                    ));
                                }
                            }
                        }
                    }
                }
                FieldType::Bytes { max_bytes }
                    if *max_bytes == 0 || *max_bytes > 16 * 1024 * 1024 =>
                {
                    return bad(format!("field {}: maxBytes must be 1..=16777216", f.name));
                }
                FieldType::Ref { to: Some(to) } | FieldType::Refs { to: Some(to) } => {
                    if !crate::uri::valid_scheme(to) {
                        return bad(format!("field {}: bad scheme {to:?} in to", f.name));
                    }
                }
                FieldType::Sequence { per } => {
                    let per = per.unwrap_or(self.scope);
                    let ok = match self.scope {
                        Scope::Global => per == Scope::Global,
                        Scope::Space => per != Scope::User,
                        Scope::User => per != Scope::Space,
                    };
                    if !ok {
                        return bad(format!(
                            "field {}: a sequence per {} needs that scope",
                            f.name,
                            per.as_str()
                        ));
                    }
                    if f.default.is_some() {
                        return bad(format!("field {}: a sequence has no default", f.name));
                    }
                }
                _ => {}
            }
            if f.transitions.is_some() && !matches!(f.ty, FieldType::Enum { .. }) {
                return bad(format!(
                    "field {}: transitions only apply to enum fields",
                    f.name
                ));
            }
            if f.min.is_some() || f.max.is_some() {
                if !matches!(f.ty, FieldType::Integer | FieldType::Number) {
                    return bad(format!(
                        "field {}: min and max only apply to integer and number fields",
                        f.name
                    ));
                }
                if let Some(min) = f.min
                    && !min.is_finite()
                {
                    return bad(format!("field {}: min must be finite", f.name));
                }
                if let Some(max) = f.max
                    && !max.is_finite()
                {
                    return bad(format!("field {}: max must be finite", f.name));
                }
                if let (Some(min), Some(max)) = (f.min, f.max)
                    && min > max
                {
                    return bad(format!("field {}: min ({min}) exceeds max ({max})", f.name));
                }
            }
            if f.min_length.is_some() || f.max_length.is_some() {
                if !matches!(
                    f.ty,
                    FieldType::Text | FieldType::Longtext | FieldType::Path
                ) {
                    return bad(format!(
                        "field {}: minLength and maxLength only apply to text, longtext and path fields",
                        f.name
                    ));
                }
                if let (Some(min_len), Some(max_len)) = (f.min_length, f.max_length)
                    && min_len > max_len
                {
                    return bad(format!(
                        "field {}: minLength ({min_len}) exceeds maxLength ({max_len})",
                        f.name
                    ));
                }
            }
            if let Some(pat) = &f.pattern {
                if !matches!(
                    f.ty,
                    FieldType::Text | FieldType::Longtext | FieldType::Path
                ) {
                    return bad(format!(
                        "field {}: pattern only applies to text, longtext and path fields",
                        f.name
                    ));
                }
                if let Err(e) = regex::Regex::new(pat) {
                    return bad(format!("field {}: invalid pattern {pat:?}: {e}", f.name));
                }
            }
            if let Some(fmt) = f.format {
                let ok = match fmt {
                    FieldFormat::Currency
                    | FieldFormat::Percent
                    | FieldFormat::Duration
                    | FieldFormat::Progress => {
                        matches!(f.ty, FieldType::Integer | FieldType::Number)
                    }
                    FieldFormat::Url | FieldFormat::Email => {
                        matches!(f.ty, FieldType::Text | FieldType::Longtext)
                    }
                };
                if !ok {
                    return bad(format!(
                        "field {}: format {} does not apply to {} fields",
                        f.name,
                        fmt.as_str(),
                        f.ty.name()
                    ));
                }
            }
            if f.search && !f.ty.is_texty() {
                return bad(format!(
                    "field {}: only text fields can be searched",
                    f.name
                ));
            }
            if f.unique
                && matches!(
                    f.ty,
                    FieldType::Richtext
                        | FieldType::Json
                        | FieldType::Refs { .. }
                        | FieldType::Bytes { .. }
                        | FieldType::Vector { .. }
                )
            {
                return bad(format!(
                    "field {}: {} fields cannot be unique",
                    f.name,
                    f.ty.name()
                ));
            }
            if f.case_insensitive
                && !(f.unique && matches!(f.ty, FieldType::Text | FieldType::Path))
            {
                return bad(format!(
                    "field {}: caseInsensitive applies to unique text fields",
                    f.name
                ));
            }
            if f.indexed() && matches!(f.ty, FieldType::Richtext | FieldType::Json) {
                return bad(format!(
                    "field {}: {} fields cannot be indexed",
                    f.name,
                    f.ty.name()
                ));
            }
            if let Some(d) = &f.default {
                super::values::input(&self.name, f, d).map_err(|e| {
                    Error::invalid(format!(
                        "collection {}: default of {}: {e}",
                        self.name, f.name
                    ))
                })?;
                super::constraints::check_field_value(&self.name, f, d).map_err(|e| {
                    Error::invalid(format!(
                        "collection {}: default of {}: {}",
                        self.name, f.name, e.message
                    ))
                })?;
            }
        }
        for f in &self.fields {
            for (what, expr) in [
                ("visibleWhen", &f.visible_when),
                ("editableWhen", &f.editable_when),
            ] {
                if let Some(v) = expr {
                    if !v.is_object() {
                        return bad(format!("field {}: {what} must be an object", f.name));
                    }
                    rookery_filter::Filter::parse(self, v).map_err(|e| {
                        Error::invalid(format!(
                            "collection {}: field {}: invalid {what}: {e}",
                            self.name, f.name
                        ))
                    })?;
                }
            }
        }
        for cols in self.indexes.iter().chain(self.unique.iter()) {
            if cols.is_empty() || cols.len() > 8 {
                return bad("an index has 1 to 8 fields".into());
            }
            for c in cols {
                match self.field(c) {
                    None => return bad(format!("index on unknown field {c}")),
                    Some(f)
                        if matches!(
                            f.ty,
                            FieldType::Richtext
                                | FieldType::Json
                                | FieldType::Refs { .. }
                                | FieldType::Bytes { .. }
                                | FieldType::Vector { .. }
                        ) =>
                    {
                        return bad(format!("field {c} cannot be in a composite index"));
                    }
                    _ => {}
                }
            }
        }
        if let Some(e) = &self.entity {
            if !crate::uri::valid_scheme(&e.scheme) {
                return bad(format!("bad entity scheme {:?}", e.scheme));
            }
            match self.field(&e.title) {
                Some(f) if f.ty.is_texty() => {}
                _ => return bad(format!("entity title {} must be a text field", e.title)),
            }
            for (what, name) in [("subtitle", &e.subtitle), ("icon", &e.icon)] {
                if let Some(n) = name {
                    match self.field(n) {
                        Some(f) if f.ty.is_texty() => {}
                        _ => return bad(format!("entity {what} {n} must be a text field")),
                    }
                }
            }
            if let Some(p) = &e.parent {
                match self.field(p) {
                    Some(FieldDef {
                        ty: FieldType::Ref { .. },
                        ..
                    }) => {}
                    _ => return bad(format!("entity parent {p} must be a ref field")),
                }
            }
            if let Some(p) = &e.private {
                match self.field(p) {
                    Some(FieldDef {
                        ty: FieldType::Boolean,
                        ..
                    }) => {}
                    _ => return bad(format!("entity private {p} must be a boolean field")),
                }
            }
        }
        Ok(())
    }
}

impl CollectionDef {
    pub fn is_log(&self) -> bool {
        self.kind == CollectionKind::Log
    }

    /// What a log collection may be: append-only rows with a per-collection `seq`, so nothing that needs a row to
    /// change, to be found again by text, or to be tracked by the kernel's indexes.
    fn validate_kind(&self) -> Result<()> {
        let bad = |msg: String| Err(Error::invalid(format!("collection {}: {msg}", self.name)));
        if !self.is_log() {
            if self.retention.is_some() || self.partition.is_some() {
                return bad("retention and partition are for log collections (kind: log)".into());
            }
            return Ok(());
        }
        if self.entity.is_some() {
            return bad("a log collection has no entities".into());
        }
        if self.history || self.trash {
            return bad("a log collection has no history or trash".into());
        }
        if !self.unique.is_empty() {
            return bad("a log collection has no unique constraints".into());
        }
        if let Some(r) = &self.retention
            && !(1..=36_500).contains(&r.days)
        {
            return bad("retention days must be 1..=36500".into());
        }
        for f in &self.fields {
            let why = match &f.ty {
                _ if f.name == "seq" => Some("seq is the log's own sequence number"),
                FieldType::Sequence { .. } => Some("a sequence field (every row has seq)"),
                FieldType::Blob => Some("a blob field (a log keeps no blob references)"),
                FieldType::Richtext => Some("a richtext field (use json)"),
                _ if f.unique => Some("a unique field"),
                _ if f.search => Some("a search field"),
                _ if f.transitions.is_some() => Some("transitions (rows are never updated)"),
                _ => None,
            };
            if let Some(why) = why {
                return bad(format!("field {}: a log collection has no {why}", f.name));
            }
        }
        Ok(())
    }
}

impl rookery_filter::Fields for CollectionDef {
    fn kind(&self, field: &str) -> Option<rookery_filter::Kind> {
        if self.is_log() {
            return match field {
                "id" | "createdBy" => Some(rookery_filter::Kind::Uuid),
                "seq" => Some(rookery_filter::Kind::Integer),
                "createdAt" => Some(rookery_filter::Kind::Datetime),
                "spaceId" if self.scope == Scope::Space => Some(rookery_filter::Kind::Uuid),
                "userId" if self.scope == Scope::User => Some(rookery_filter::Kind::Uuid),
                _ => self.field(field).map(|f| super::values::kind_of(&f.ty)),
            };
        }
        let sys = match field {
            "id" => Some(rookery_filter::Kind::Uuid),
            "spaceId" if self.scope == Scope::Space => Some(rookery_filter::Kind::Uuid),
            "userId" if self.scope == Scope::User => Some(rookery_filter::Kind::Uuid),
            "createdAt" | "updatedAt" => Some(rookery_filter::Kind::Datetime),
            "deletedAt" if self.trash => Some(rookery_filter::Kind::Datetime),
            "createdBy" | "updatedBy" => Some(rookery_filter::Kind::Uuid),
            "rev" => Some(rookery_filter::Kind::Integer),
            _ => None,
        };
        if sys.is_some() {
            return sys;
        }
        self.field(field).map(|f| super::values::kind_of(&f.ty))
    }
}
