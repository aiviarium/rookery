//! Aggregates: grouped counts, sums and extremes over the rows a caller may read.
//!
//! The statement is the same read `find` does (scope, trash and access, then the filter), grouped. Scope and
//! access are conditions of the rows that are aggregated, so a group never counts a row the caller cannot read.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgConnection, Postgres, QueryBuilder, Row};
use uuid::Uuid;

use super::Collection;
use super::filter::Filter;
use super::query::{Direction, OrderBy, plan_read, push_read_where};
use super::schema::valid_ident;
use super::values::Kind;
use crate::caller::Caller;
use crate::error::{Error, Result};

/// The most groups one aggregate returns.
pub const MAX_GROUPS: i64 = 1000;
pub const MAX_GROUP_KEYS: usize = 3;
pub const MAX_MEASURES: usize = 20;

/// `groupBy` on a date or datetime field: the start of the day, week (Monday) or month, in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Bucket {
    Day,
    Week,
    Month,
}

impl Bucket {
    fn as_str(self) -> &'static str {
        match self {
            Bucket::Day => "day",
            Bucket::Week => "week",
            Bucket::Month => "month",
        }
    }
}

/// One `groupBy` key: `"field"` or `{ field, by }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GroupBy {
    Field(String),
    Bucketed { field: String, by: Bucket },
}

impl GroupBy {
    fn field(&self) -> &str {
        match self {
            GroupBy::Field(f) | GroupBy::Bucketed { field: f, .. } => f,
        }
    }
}

/// One measure: exactly one function, with its field (`count` takes `true`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Measure {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count_distinct: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sum: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<String>,
}

/// An aggregate over one collection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregateQuery {
    #[serde(default, rename = "where", skip_serializing_if = "Option::is_none")]
    pub filter: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_by: Vec<GroupBy>,
    pub measures: BTreeMap<String, Measure>,
    /// Group keys (by field name) and measures (by name); default: the group keys, ascending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_by: Option<Vec<OrderBy>>,
    /// Keep the first groups in order. Without it, more than 1,000 groups is an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Space-scoped collections: these spaces instead of the caller's current one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spaces: Option<Vec<Uuid>>,
}

/// A checked key or measure: its SQL expression and the name it has in the result.
struct Column {
    name: String,
    sql: String,
}

fn kind_of(coll: &Collection, field: &str, what: &str) -> Result<(String, Kind)> {
    coll.column(field)
        .ok_or_else(|| Error::invalid(format!("{what}: {} has no field {field}", coll.def.name)))
}

fn group_column(coll: &Collection, g: &GroupBy) -> Result<Column> {
    let field = g.field();
    let (col, kind) = kind_of(coll, field, "groupBy")?;
    let sql = match (g, &kind) {
        (_, Kind::Json | Kind::Richtext | Kind::Refs(_) | Kind::Bytes | Kind::Vector) => {
            return Err(Error::invalid(format!("groupBy: cannot group by {field}")));
        }
        (GroupBy::Field(_), _) => col,
        (GroupBy::Bucketed { by, .. }, Kind::Date) => {
            format!("date_trunc('{}', {col}::timestamp)::date", by.as_str())
        }
        (GroupBy::Bucketed { by, .. }, Kind::Datetime) => {
            format!(
                "date_trunc('{}', {col} AT TIME ZONE 'UTC')::date",
                by.as_str()
            )
        }
        (GroupBy::Bucketed { .. }, _) => {
            return Err(Error::invalid(format!(
                "groupBy: only date and datetime fields can be bucketed, {field} cannot"
            )));
        }
    };
    Ok(Column {
        name: field.to_string(),
        sql,
    })
}

fn measure_column(coll: &Collection, name: &str, m: &Measure) -> Result<Column> {
    if !valid_ident(name, 63) {
        return Err(Error::invalid(format!(
            "measures: {name:?} is not a valid name"
        )));
    }
    let given: Vec<&str> = [
        m.count.map(|_| "count"),
        m.count_distinct.as_ref().map(|_| "countDistinct"),
        m.sum.as_ref().map(|_| "sum"),
        m.avg.as_ref().map(|_| "avg"),
        m.min.as_ref().map(|_| "min"),
        m.max.as_ref().map(|_| "max"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if given.len() != 1 {
        return Err(Error::invalid(format!(
            "measures: {name} takes exactly one of count, countDistinct, sum, avg, min and max"
        )));
    }
    let bad = |function: &str, field: &str, why: &str| {
        Error::invalid(format!("measures: {name}: {function} of {field} {why}"))
    };
    let sql = if let Some(on) = m.count {
        if !on {
            return Err(Error::invalid(format!(
                "measures: {name}: count takes true"
            )));
        }
        "count(*)".to_string()
    } else if let Some(f) = &m.count_distinct {
        let (col, kind) = kind_of(coll, f, "measures")?;
        if matches!(
            kind,
            Kind::Json | Kind::Richtext | Kind::Refs(_) | Kind::Bytes | Kind::Vector
        ) {
            return Err(bad("countDistinct", f, "is not possible"));
        }
        format!("count(DISTINCT {col})")
    } else if let Some(f) = &m.sum {
        let (col, kind) = kind_of(coll, f, "measures")?;
        match kind {
            Kind::Integer | Kind::Number => format!("sum({col})"),
            _ => return Err(bad("sum", f, "needs an integer or number field")),
        }
    } else if let Some(f) = &m.avg {
        let (col, kind) = kind_of(coll, f, "measures")?;
        match kind {
            Kind::Integer | Kind::Number => format!("avg({col})::float8"),
            _ => return Err(bad("avg", f, "needs an integer or number field")),
        }
    } else {
        let (function, f) = match (&m.min, &m.max) {
            (Some(f), _) => ("min", f),
            (_, Some(f)) => ("max", f),
            _ => unreachable!("one function is given"),
        };
        let (col, kind) = kind_of(coll, f, "measures")?;
        match kind {
            Kind::Integer
            | Kind::Number
            | Kind::Date
            | Kind::Datetime
            | Kind::Text
            | Kind::Enum(_)
            | Kind::Ref(_) => format!("{function}({col})"),
            _ => {
                return Err(bad(
                    function,
                    f,
                    "needs a number, date, datetime or text field",
                ));
            }
        }
    };
    Ok(Column {
        name: name.to_string(),
        sql,
    })
}

pub(crate) async fn aggregate(
    conn: &mut PgConnection,
    coll: &Collection,
    caller: &Caller,
    q: &AggregateQuery,
) -> Result<Vec<Value>> {
    if q.group_by.len() > MAX_GROUP_KEYS {
        return Err(Error::invalid(format!(
            "groupBy takes at most {MAX_GROUP_KEYS} keys"
        )));
    }
    if q.measures.is_empty() || q.measures.len() > MAX_MEASURES {
        return Err(Error::invalid(format!(
            "measures takes 1 to {MAX_MEASURES} measures"
        )));
    }
    if let Some(l) = q.limit
        && !(1..=MAX_GROUPS).contains(&l)
    {
        return Err(Error::invalid(format!(
            "limit must be between 1 and {MAX_GROUPS}"
        )));
    }
    let keys = q
        .group_by
        .iter()
        .map(|g| group_column(coll, g))
        .collect::<Result<Vec<_>>>()?;
    let measures = q
        .measures
        .iter()
        .map(|(name, m)| measure_column(coll, name, m))
        .collect::<Result<Vec<_>>>()?;
    for (i, k) in keys.iter().enumerate() {
        if keys[..i].iter().any(|o| o.name == k.name) || measures.iter().any(|m| m.name == k.name) {
            return Err(Error::invalid(format!(
                "{} is used twice as a group key or measure name",
                k.name
            )));
        }
    }
    let filter = match &q.filter {
        Some(f) => Some(Filter::parse(coll, f)?),
        None => None,
    };
    // The order: what was asked for, then the group keys, so the result is the same on every run.
    let mut order: Vec<(&Column, Direction)> = Vec::new();
    for o in q.order_by.iter().flatten() {
        let (name, dir) = match o {
            OrderBy::Field(f) => (f.as_str(), Direction::Asc),
            OrderBy::Pair(f, d) => (f.as_str(), *d),
        };
        let col = keys
            .iter()
            .chain(measures.iter())
            .find(|c| c.name == name)
            .ok_or_else(|| {
                Error::invalid(format!("orderBy: {name} is not a group key or a measure"))
            })?;
        order.push((col, dir));
    }
    for k in &keys {
        if !order.iter().any(|(c, _)| c.name == k.name) {
            order.push((k, Direction::Asc));
        }
    }
    let plan = plan_read(conn, coll, caller, q.spaces.as_deref(), false).await?;

    let mut qb = QueryBuilder::<Postgres>::new("SELECT jsonb_build_object(");
    for (i, c) in keys.iter().chain(measures.iter()).enumerate() {
        if i > 0 {
            qb.push(", ");
        }
        qb.push_bind(c.name.clone());
        qb.push("::text, ");
        qb.push(&c.sql);
    }
    qb.push(format!(") AS row FROM {} t", coll.table));
    push_read_where(&mut qb, coll, caller, &plan);
    if let Some(f) = &filter {
        qb.push(" AND ");
        f.push_sql(coll, &mut qb);
    }
    if !keys.is_empty() {
        qb.push(" GROUP BY ");
        qb.push(
            keys.iter()
                .map(|k| k.sql.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        );
        qb.push(" ORDER BY ");
        let parts: Vec<String> = order
            .iter()
            .map(|(c, d)| {
                format!(
                    "{} {}",
                    c.sql,
                    if *d == Direction::Desc { "DESC" } else { "ASC" }
                )
            })
            .collect();
        qb.push(parts.join(", "));
        qb.push(" LIMIT ");
        qb.push_bind(q.limit.unwrap_or(MAX_GROUPS + 1));
    }
    let rows: Vec<Value> = qb
        .build()
        .fetch_all(&mut *conn)
        .await?
        .iter()
        .map(|r| r.try_get("row"))
        .collect::<std::result::Result<_, _>>()?;
    if q.limit.is_none() && rows.len() as i64 > MAX_GROUPS {
        return Err(Error::invalid(format!(
            "more than {MAX_GROUPS} groups: narrow the filter, group by fewer keys, or order the groups and set a limit"
        )));
    }
    Ok(rows)
}
