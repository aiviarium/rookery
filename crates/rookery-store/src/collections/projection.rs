//! Which columns a read returns (`select`).
//!
//! The system columns are always returned. Declared fields are returned when `select` names them; without
//! `select`, every declared field except `bytes` and `vector` ones, which are large and are only read when named
//! (`get` and the rows a write returns have them all). The columns are chosen in SQL, so a field that is not
//! returned is not read from the table's rows.

use serde_json::Value;
use sqlx::{Postgres, QueryBuilder};

use super::Collection;
use super::schema::{FieldDef, FieldType};
use super::values::row_out_fields;
use crate::error::{Error, Result};
use crate::util::quote_ident;

pub(crate) struct Projection {
    /// The declared fields in the select list: the selected ones, then `extra`.
    fields: Vec<String>,
    /// Fields read only so the caller can build a cursor from the row; removed from the result.
    extra: Vec<String>,
    columns: String,
}

fn is_binary(f: &FieldDef) -> bool {
    matches!(f.ty, FieldType::Bytes { .. } | FieldType::Vector { .. })
}

impl Projection {
    /// `select` is the option as given (None: the default set). `extra` names declared fields the query needs
    /// beside the selection; they are read and then stripped by [`Projection::strip_extra`].
    pub fn new(
        coll: &Collection,
        select: Option<&[String]>,
        extra: &[String],
    ) -> Result<Projection> {
        let mut fields: Vec<String> = Vec::new();
        match select {
            None => fields.extend(
                coll.def
                    .fields
                    .iter()
                    .filter(|f| !is_binary(f))
                    .map(|f| f.name.clone()),
            ),
            Some(names) => {
                for name in names {
                    if coll.def.field(name).is_some() {
                        // Declaration order, whatever order `select` names them in.
                        continue;
                    }
                    if coll.column(name).is_none() {
                        return Err(Error::invalid(format!(
                            "select: {} has no field {name}",
                            coll.def.name
                        )));
                    }
                }
                fields.extend(
                    coll.def
                        .fields
                        .iter()
                        .filter(|f| names.iter().any(|n| n == &f.name))
                        .map(|f| f.name.clone()),
                );
            }
        }
        let mut added = Vec::new();
        for name in extra {
            if coll.def.field(name).is_some() && !fields.contains(name) && !added.contains(name) {
                added.push(name.clone());
            }
        }
        fields.extend(added.iter().cloned());
        let mut columns: Vec<String> = coll
            .system_columns()
            .into_iter()
            .map(|c| format!("t.{c}"))
            .collect();
        columns.extend(fields.iter().map(|f| format!("t.{}", quote_ident(f))));
        Ok(Projection {
            fields,
            extra: added,
            columns: columns.join(", "),
        })
    }

    /// Pushes the expression that yields a row as `jsonb`.
    pub fn push(&self, qb: &mut QueryBuilder<Postgres>) {
        qb.push("(SELECT to_jsonb(p) FROM (SELECT ");
        qb.push(&self.columns);
        qb.push(") p)");
    }

    /// The API form of a row read with this projection.
    pub fn row(&self, coll: &Collection, v: Value) -> Value {
        row_out_fields(coll, v, |f| self.fields.contains(&f.name))
    }

    pub fn strip_extra(&self, row: &mut Value) {
        if let Some(m) = row.as_object_mut() {
            for name in &self.extra {
                m.remove(name);
            }
        }
    }
}

impl Collection {
    /// A row as `select` would have returned it: system fields, and the declared fields `select` names (all but
    /// `bytes` and `vector` without it). Used for rows that were read whole, such as a live query's changes.
    pub fn project(&self, mut row: Value, select: Option<&[String]>) -> Value {
        if let Some(m) = row.as_object_mut() {
            for f in &self.def.fields {
                let keep = match select {
                    None => !is_binary(f),
                    Some(names) => names.iter().any(|n| n == &f.name),
                };
                if !keep {
                    m.remove(&f.name);
                }
            }
        }
        row
    }

    /// The SQL system columns a read returns, as quoted names without the table alias.
    pub(crate) fn system_columns(&self) -> Vec<&'static str> {
        let mut cols = if self.def.is_log() {
            vec!["seq", "id"]
        } else {
            vec!["id"]
        };
        match self.def.scope {
            super::Scope::Space => cols.push("space_id"),
            super::Scope::User => cols.push("user_id"),
            super::Scope::Global => {}
        }
        if self.def.is_log() {
            cols.extend(["created_at", "created_by"]);
            return cols;
        }
        cols.extend([
            "created_at",
            "updated_at",
            "created_by",
            "updated_by",
            "rev",
        ]);
        if self.def.trash {
            cols.push("deleted_at");
        }
        cols
    }
}
