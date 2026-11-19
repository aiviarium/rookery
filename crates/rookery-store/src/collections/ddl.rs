//! Tables and indexes for collections, and the additive diff between a stored definition and
//! a manifest's.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use super::Collection;
use super::schema::{CollectionDef, FieldDef, FieldType, Scope};
use crate::error::SchemaDifference;
use crate::util::quote_ident;

/// An index derived from a collection definition. The name hashes the definition, so a
/// changed index has a new name and the diff is a set difference.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IndexSpec {
    pub name: String,
    pub create: String,
}

fn scope_cols(scope: Scope) -> Vec<String> {
    match scope {
        Scope::Global => vec![],
        Scope::Space => vec!["space_id".into()],
        Scope::User => vec!["user_id".into()],
    }
}

fn spec(
    coll: &Collection,
    unique: bool,
    method: &str,
    cols: &[String],
    partial: bool,
) -> IndexSpec {
    let body = format!("{unique}|{method}|{}|{partial}", cols.join(","));
    let h = hex::encode(&Sha256::digest(body.as_bytes())[..5]);
    let name = format!("{}__{h}", coll.def.name);
    let create = format!(
        "CREATE {}INDEX IF NOT EXISTS {} ON {} USING {method} ({}){}",
        if unique { "UNIQUE " } else { "" },
        quote_ident(&name),
        coll.table,
        cols.join(", "),
        if partial {
            " WHERE deleted_at IS NULL"
        } else {
            ""
        }
    );
    IndexSpec { name, create }
}

/// The indexes a definition implies, beside the primary key and the base indexes.
pub(crate) fn index_specs(coll: &Collection) -> Vec<IndexSpec> {
    let def = &coll.def;
    let scope = scope_cols(def.scope);
    let mut out = Vec::new();
    for f in &def.fields {
        let col = quote_ident(&f.name);
        match &f.ty {
            FieldType::Vector { metric, .. }
                if f.vector_index() == Some(super::schema::VectorIndex::Hnsw) =>
            {
                let ops = match metric {
                    super::schema::VectorMetric::Cosine => "vector_cosine_ops",
                    super::schema::VectorMetric::L2 => "vector_l2_ops",
                    super::schema::VectorMetric::Ip => "vector_ip_ops",
                };
                out.push(spec(coll, false, "hnsw", &[format!("{col} {ops}")], false));
            }
            FieldType::Refs { .. } => {
                out.push(spec(coll, false, "gin", std::slice::from_ref(&col), false))
            }
            FieldType::Ref { .. } => {
                let mut cols = scope.clone();
                cols.push(col.clone());
                out.push(spec(coll, false, "btree", &cols, false));
            }
            FieldType::Sequence { per } => {
                let mut cols = scope_cols(per.unwrap_or(def.scope));
                cols.push(col.clone());
                out.push(spec(coll, true, "btree", &cols, false));
            }
            _ if f.indexed() => {
                let mut cols = scope.clone();
                cols.push(col.clone());
                out.push(spec(coll, false, "btree", &cols, false));
            }
            _ => {}
        }
        if f.unique && !matches!(f.ty, FieldType::Sequence { .. }) {
            let mut cols = scope.clone();
            cols.push(if f.case_insensitive {
                format!("lower({col})")
            } else {
                col.clone()
            });
            out.push(spec(coll, true, "btree", &cols, true));
        }
    }
    for idx in &def.indexes {
        let mut cols = scope.clone();
        cols.extend(idx.iter().map(|c| quote_ident(c)));
        out.push(spec(coll, false, "btree", &cols, false));
    }
    for idx in &def.unique {
        let mut cols = scope.clone();
        cols.extend(idx.iter().map(|c| quote_ident(c)));
        out.push(spec(coll, true, "btree", &cols, true));
    }
    out.dedup_by(|a, b| a.name == b.name);
    out
}

pub(crate) fn drop_index_sql(coll: &Collection, name: &str) -> String {
    format!(
        "DROP INDEX IF EXISTS {}.{}",
        quote_ident(&coll.schema),
        quote_ident(name)
    )
}

pub(crate) fn column_sql(f: &FieldDef) -> String {
    format!("{} {}", quote_ident(&f.name), f.ty.sql_type())
}

/// The sequence behind a log collection's `seq`.
pub(crate) fn log_sequence(coll: &Collection) -> String {
    format!(
        "{}.{}",
        quote_ident(&coll.schema),
        quote_ident(&format!("{}__seq", coll.def.name))
    )
}

/// A log collection: a table partitioned by `created_at` (partitions are made as time passes, see `log.rs`)
/// whose `seq` comes from a sequence of its own, and indexes for reading in order, by id and by scope.
fn create_log_table(coll: &Collection) -> Vec<String> {
    let mut cols = vec![
        format!(
            "seq bigint NOT NULL DEFAULT nextval('{}')",
            log_sequence(coll).replace('\'', "''")
        ),
        "id uuid NOT NULL".to_string(),
        "space_id uuid".into(),
        "user_id uuid".into(),
        "created_at timestamptz NOT NULL DEFAULT now()".into(),
        "created_by uuid".into(),
    ];
    cols.extend(coll.def.fields.iter().map(column_sql));
    let mut out = vec![
        format!("CREATE SEQUENCE {}", log_sequence(coll)),
        format!(
            "CREATE TABLE {} ({}) PARTITION BY RANGE (created_at)",
            coll.table,
            cols.join(", ")
        ),
        format!(
            "CREATE INDEX {} ON {} (seq)",
            quote_ident(&format!("{}__seq_idx", coll.def.name)),
            coll.table
        ),
        format!(
            "CREATE INDEX {} ON {} (id)",
            quote_ident(&format!("{}__id", coll.def.name)),
            coll.table
        ),
    ];
    let scope = scope_cols(coll.def.scope);
    if !scope.is_empty() {
        out.push(format!(
            "CREATE INDEX {} ON {} ({}, seq)",
            quote_ident(&format!("{}__scope", coll.def.name)),
            coll.table,
            scope.join(", ")
        ));
    }
    out.extend(index_specs(coll).into_iter().map(|s| s.create));
    out
}

/// `CREATE TABLE` and the base indexes.
pub(crate) fn create_table(coll: &Collection) -> Vec<String> {
    if coll.def.is_log() {
        return create_log_table(coll);
    }
    let mut cols = vec![
        "id uuid PRIMARY KEY".to_string(),
        "space_id uuid".into(),
        "user_id uuid".into(),
        "created_at timestamptz NOT NULL DEFAULT now()".into(),
        "updated_at timestamptz NOT NULL DEFAULT now()".into(),
        "created_by uuid".into(),
        "updated_by uuid".into(),
        "rev bigint NOT NULL DEFAULT 1".into(),
        "deleted_at timestamptz".into(),
        "_search tsvector".into(),
    ];
    cols.extend(coll.def.fields.iter().map(column_sql));
    let mut out = vec![format!("CREATE TABLE {} ({})", coll.table, cols.join(", "))];
    let scope = scope_cols(coll.def.scope);
    if !scope.is_empty() {
        out.push(format!(
            "CREATE INDEX {} ON {} ({}, id)",
            quote_ident(&format!("{}__scope", coll.def.name)),
            coll.table,
            scope.join(", ")
        ));
    }
    out.push(format!(
        "CREATE INDEX {} ON {} USING gin (_search)",
        quote_ident(&format!("{}__search", coll.def.name)),
        coll.table
    ));
    out.extend(index_specs(coll).into_iter().map(|s| s.create));
    out
}

/// What applying a manifest does to one existing collection.
#[derive(Debug, Default, Clone)]
pub(crate) struct AlterPlan {
    pub add_fields: Vec<FieldDef>,
    pub create_indexes: Vec<IndexSpec>,
    pub drop_indexes: Vec<String>,
    pub reindex_search: bool,
    pub reindex_entities: bool,
    /// References change their source (the rows became entities).
    pub reindex_refs: bool,
    /// Human-readable list of what changes.
    pub notes: Vec<String>,
}

/// The additive changes from `old` to `new`, or every non-additive difference.
pub(crate) fn diff(old: &Collection, new: &Collection) -> Result<AlterPlan, Vec<SchemaDifference>> {
    let (o, n) = (&old.def, &new.def);
    let mut diffs = Vec::new();
    let mut plan = AlterPlan::default();
    let d = |field: Option<&str>, description: String| SchemaDifference {
        collection: n.name.clone(),
        field: field.map(str::to_string),
        description,
    };
    if o.scope != n.scope {
        diffs.push(d(
            None,
            format!(
                "scope changed from {} to {}",
                o.scope.as_str(),
                n.scope.as_str()
            ),
        ));
    }
    match (&o.entity, &n.entity) {
        (Some(_), None) => diffs.push(d(None, "is no longer an entity collection".into())),
        (Some(a), Some(b)) if a.scheme != b.scheme => diffs.push(d(
            None,
            format!("entity scheme changed from {} to {}", a.scheme, b.scheme),
        )),
        (Some(a), Some(b)) if a != b => {
            plan.reindex_entities = true;
            plan.notes.push("entity options changed".into());
        }
        (None, Some(_)) => {
            plan.reindex_entities = true;
            plan.reindex_refs = true;
            plan.notes.push("became an entity collection".into());
        }
        _ => {}
    }
    if o.kind != n.kind {
        diffs.push(d(
            None,
            "kind changed; a collection is regular or a log for good".into(),
        ));
    }
    if o.partition != n.partition && n.is_log() {
        diffs.push(d(None, "partition changed".into()));
    }
    if o.trash && !n.trash {
        diffs.push(d(
            None,
            "trash was turned off; empty it with a purgeTrash migration first".into(),
        ));
    }
    let new_fields: BTreeMap<&str, &FieldDef> =
        n.fields.iter().map(|f| (f.name.as_str(), f)).collect();
    for of in &o.fields {
        let Some(nf) = new_fields.get(of.name.as_str()) else {
            diffs.push(d(
                Some(&of.name),
                "was removed; drop it with a migration".into(),
            ));
            continue;
        };
        match (&of.ty, &nf.ty) {
            (FieldType::Enum { values: a }, FieldType::Enum { values: b }) => {
                let removed: Vec<&str> = a
                    .iter()
                    .map(|v| v.value())
                    .filter(|v| !b.iter().any(|nb| nb.value() == *v))
                    .collect();
                if !removed.is_empty() {
                    diffs.push(d(
                        Some(&of.name),
                        format!(
                            "enum values were removed ({}); migrate the rows first",
                            removed.join(", ")
                        ),
                    ));
                }
            }
            (FieldType::Vector { dimensions: a, .. }, FieldType::Vector { dimensions: b, .. })
                if a != b =>
            {
                diffs.push(d(
                    Some(&of.name),
                    "vector dimensions changed; migrate the rows first".into(),
                ));
            }
            (FieldType::Ref { to: a }, FieldType::Ref { to: b })
            | (FieldType::Refs { to: a }, FieldType::Refs { to: b })
                if a != b =>
            {
                diffs.push(d(
                    Some(&of.name),
                    format!("reference target changed from {a:?} to {b:?}"),
                ));
            }
            (FieldType::Sequence { per: a }, FieldType::Sequence { per: b })
                if a.unwrap_or(o.scope) != b.unwrap_or(n.scope) =>
            {
                diffs.push(d(Some(&of.name), "sequence scope changed".into()));
            }
            (a, b) if std::mem::discriminant(a) != std::mem::discriminant(b) => {
                diffs.push(d(
                    Some(&of.name),
                    format!(
                        "type changed from {} to {}; change it with a migration",
                        a.name(),
                        b.name()
                    ),
                ));
            }
            _ => {}
        }
        if of.search != nf.search {
            plan.reindex_search = true;
            plan.notes.push(format!("search on {} changed", of.name));
        }
    }
    for nf in &n.fields {
        if o.field(&nf.name).is_none() {
            plan.notes.push(format!("add field {}", nf.name));
            if nf.search {
                plan.reindex_search = true;
            }
            plan.add_fields.push(nf.clone());
        }
    }
    if !diffs.is_empty() {
        return Err(diffs);
    }
    let old_idx = index_specs(old);
    let new_idx = index_specs(new);
    for s in &old_idx {
        if !new_idx.iter().any(|x| x.name == s.name) {
            plan.notes.push(format!("drop index {}", s.name));
            plan.drop_indexes.push(s.name.clone());
        }
    }
    for s in &new_idx {
        if !old_idx.iter().any(|x| x.name == s.name) {
            plan.notes.push(format!("create index {}", s.name));
            plan.create_indexes.push(s.clone());
        }
    }
    Ok(plan)
}

/// Collections in `stored` that `wanted` no longer declares.
pub(crate) fn removed_collections(
    stored: &[CollectionDef],
    wanted: &[CollectionDef],
) -> Vec<SchemaDifference> {
    stored
        .iter()
        .filter(|s| !wanted.iter().any(|w| w.name == s.name))
        .map(|s| SchemaDifference {
            collection: s.name.clone(),
            field: None,
            description: "was removed; drop it with a migration".into(),
        })
        .collect()
}
