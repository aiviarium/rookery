//! `include`: the rows that `ref` and `refs` fields of a page point to, fetched with the page.
//!
//! One level deep. The references of the page are collected, grouped by the scheme of the collection they name
//! (any plugin's), and read with one statement per collection: scope, trash and access are those of an ordinary
//! read of that collection by the same caller, so a row the caller may not read is left out, as is a reference
//! to anything that is not a collection row. The result is keyed by the reference's value, an entity URI.

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use sqlx::{PgConnection, Postgres, QueryBuilder, Row};
use uuid::Uuid;

use super::projection::Projection;
use super::query::{plan_read, push_read_where};
use super::schema::FieldType;
use super::{Collection, Collections};
use crate::caller::Caller;
use crate::error::{Error, Result};
use rookery_proto::ErrorCode;

/// The most fields `include` names.
pub const MAX_INCLUDE: usize = 5;

/// Checks `include` against the collection: up to five distinct `ref` or `refs` fields, all returned by the read.
pub(crate) fn check(
    coll: &Collection,
    include: &[String],
    select: Option<&[String]>,
) -> Result<()> {
    if include.len() > MAX_INCLUDE {
        return Err(Error::invalid(format!(
            "include takes at most {MAX_INCLUDE} fields"
        )));
    }
    for (i, name) in include.iter().enumerate() {
        if include[..i].contains(name) {
            return Err(Error::invalid(format!("include names {name} twice")));
        }
        match coll.def.field(name).map(|f| &f.ty) {
            Some(FieldType::Ref { .. } | FieldType::Refs { .. }) => {}
            Some(ty) => {
                return Err(Error::invalid(format!(
                    "include: field {name} is {}, not a ref or refs",
                    ty.name()
                )));
            }
            None => {
                return Err(Error::invalid(format!(
                    "include: {} has no field {name}",
                    coll.def.name
                )));
            }
        }
        if select.is_some_and(|s| !s.contains(name)) {
            return Err(Error::invalid(format!(
                "include: field {name} must be in select"
            )));
        }
    }
    Ok(())
}

/// The distinct reference values of `fields` in `rows`.
fn references(rows: &[Value], fields: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in rows {
        for f in fields {
            let values: Vec<&str> = match row.get(f) {
                Some(Value::String(s)) => vec![s.as_str()],
                Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
                _ => vec![],
            };
            for v in values {
                if seen.insert(v.to_string()) {
                    out.push(v.to_string());
                }
            }
        }
    }
    out
}

/// Reads the rows `rows` reference through `fields`.
pub(crate) async fn fetch(
    collections: &Collections<'_>,
    conn: &mut PgConnection,
    caller: &Caller,
    rows: &[Value],
    fields: &[String],
    spaces: Option<&[Uuid]>,
) -> Result<Map<String, Value>> {
    // scheme -> (row id -> the URIs that name it)
    let mut by_scheme: BTreeMap<String, BTreeMap<Uuid, String>> = BTreeMap::new();
    for uri in references(rows, fields) {
        let Some((scheme, id)) = uri.split_once("://") else {
            continue;
        };
        let Ok(id) = Uuid::parse_str(id) else {
            continue;
        };
        by_scheme
            .entry(scheme.to_string())
            .or_default()
            .insert(id, uri.clone());
    }
    let mut included = Map::new();
    for (scheme, ids) in by_scheme {
        let Some(target) = collections.by_scheme(&scheme) else {
            continue;
        };
        let plan = match plan_read(conn, &target, caller, spaces_for(&target, spaces), false).await
        {
            Ok(plan) => plan,
            // Not readable by the caller, or a scope the caller has no context for: nothing to show.
            Err(e) if matches!(e.code(), ErrorCode::Forbidden | ErrorCode::Invalid) => continue,
            Err(e) => return Err(e),
        };
        let projection = Projection::new(&target, None, &[])?;
        let mut qb = QueryBuilder::<Postgres>::new("SELECT ");
        projection.push(&mut qb);
        qb.push(format!(" AS row FROM {} t", target.table));
        push_read_where(&mut qb, &target, caller, &plan);
        qb.push(" AND t.id = ANY(");
        qb.push_bind(ids.keys().copied().collect::<Vec<_>>());
        qb.push(")");
        for r in qb.build().fetch_all(&mut *conn).await? {
            let row = projection.row(&target, r.try_get("row")?);
            let id = row
                .get("id")
                .and_then(Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok());
            if let Some(uri) = id.and_then(|id| ids.get(&id)) {
                included.insert(uri.clone(), row);
            }
        }
    }
    Ok(included)
}

/// The spaces a read of `target` covers: the query's own when the target is space-scoped.
fn spaces_for<'a>(target: &Collection, spaces: Option<&'a [Uuid]>) -> Option<&'a [Uuid]> {
    match target.def.scope {
        super::Scope::Space => spaces,
        _ => None,
    }
}
