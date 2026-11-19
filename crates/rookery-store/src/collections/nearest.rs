//! Vector search (`nearest`): the rows closest to a query vector by the field's metric.
//!
//! The metric is the field's, so the operator matches the declared HNSW index (`<=>` cosine, `<->` l2,
//! `<#>` inner product; pgvector's inner-product operator returns the negative product, and so does
//! `_distance`). The filter, scope and access rules are conditions of the same statement: Postgres applies
//! them to what the index yields, and pgvector's iterative scan (0.8, `strict_order`) keeps scanning until
//! `limit` rows pass them or `hnsw.max_scan_tuples` is reached, so a selective filter can return fewer than
//! `limit` rows even when more exist. Rows with no vector are not candidates. Ties in distance are ordered by `id`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, Postgres, QueryBuilder, Row};

use super::Collection;
use super::filter::Filter;
use super::projection::Projection;
use super::query::{DEFAULT_LIMIT, Page, Query, plan_read, push_read_where};
use super::schema::{FieldType, VectorMetric};
use super::values::{Bind, SqlBind};
use crate::caller::Caller;
use crate::error::{Error, Result};
use crate::util::quote_ident;

/// The most rows one `nearest` query returns.
pub const MAX_NEAREST_LIMIT: i64 = 200;

/// `nearest: { field, to, maxDistance? }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Nearest {
    /// A `vector` field.
    pub field: String,
    /// The query vector, with the field's dimensions.
    pub to: Vec<f32>,
    /// Only rows at most this far away (in the field's metric).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_distance: Option<f64>,
}

/// A checked `nearest`: the SQL operator and the bound vector text.
pub(crate) struct Checked {
    column: String,
    op: &'static str,
    vector: String,
    max_distance: Option<f64>,
    pub limit: i64,
}

/// Checks a query's `nearest` against the collection and the rest of the query.
pub(crate) fn check(coll: &Collection, q: &Query) -> Result<Checked> {
    let n = q.nearest.as_ref().expect("nearest");
    if q.order_by.as_ref().is_some_and(|o| !o.is_empty()) || q.after.is_some() || q.search.is_some()
    {
        return Err(Error::invalid("nearest replaces orderBy, after and search"));
    }
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_NEAREST_LIMIT).contains(&limit) {
        return Err(Error::invalid(format!(
            "limit must be between 1 and {MAX_NEAREST_LIMIT} with nearest"
        )));
    }
    let Some(field) = coll.def.field(&n.field) else {
        return Err(Error::invalid(format!(
            "nearest: {} has no field {}",
            coll.def.name, n.field
        )));
    };
    let FieldType::Vector { dimensions, metric } = &field.ty else {
        return Err(Error::invalid(format!(
            "nearest: field {} is {}, not a vector",
            field.name,
            field.ty.name()
        )));
    };
    if n.to.len() != *dimensions as usize {
        return Err(Error::invalid(format!(
            "nearest: field {} has {dimensions} dimensions, the query vector has {}",
            field.name,
            n.to.len()
        )));
    }
    if n.to.iter().any(|x| !x.is_finite()) || n.max_distance.is_some_and(|d| !d.is_finite()) {
        return Err(Error::invalid("nearest: numbers must be finite"));
    }
    Ok(Checked {
        column: format!("t.{}", quote_ident(&field.name)),
        op: match metric {
            VectorMetric::Cosine => "<=>",
            VectorMetric::L2 => "<->",
            VectorMetric::Ip => "<#>",
        },
        vector: serde_json::to_string(&n.to).map_err(|e| Error::invalid(e.to_string()))?,
        max_distance: n.max_distance,
        limit,
    })
}

/// Pushes the statement. The caller has made `plan` for the collection and the query's spaces.
pub(crate) fn push_sql(
    qb: &mut QueryBuilder<Postgres>,
    coll: &Collection,
    caller: &Caller,
    plan: &super::query::ReadPlan,
    filter: Option<&Filter>,
    projection: &Projection,
    n: &Checked,
) {
    qb.push("SELECT ");
    projection.push(qb);
    qb.push(" AS row, (");
    qb.push(&n.column);
    qb.push(format!(" {} ", n.op));
    Bind::Text(n.vector.clone()).push(qb, "vector");
    qb.push(")::float8 AS _distance FROM ");
    qb.push(&coll.table);
    qb.push(" t");
    push_read_where(qb, coll, caller, plan);
    qb.push(format!(" AND {} IS NOT NULL", n.column));
    if let Some(f) = filter {
        qb.push(" AND ");
        f.push_sql(coll, qb);
    }
    if let Some(max) = n.max_distance {
        qb.push(" AND (");
        qb.push(&n.column);
        qb.push(format!(" {} ", n.op));
        Bind::Text(n.vector.clone()).push(qb, "vector");
        qb.push(") <= ");
        qb.push_bind(max);
    }
    qb.push(" ORDER BY _distance, t.id LIMIT ");
    qb.push_bind(n.limit);
}

pub(crate) async fn find(
    conn: &mut PgConnection,
    coll: &Collection,
    caller: &Caller,
    q: &Query,
) -> Result<Page> {
    let n = check(coll, q)?;
    let filter = match &q.filter {
        Some(f) => Some(Filter::parse(coll, f)?),
        None => None,
    };
    let projection = Projection::new(coll, q.select.as_deref(), &[])?;
    let plan = plan_read(conn, coll, caller, q.spaces.as_deref(), q.trashed).await?;
    let mut qb = QueryBuilder::<Postgres>::new("");
    push_sql(
        &mut qb,
        coll,
        caller,
        &plan,
        filter.as_ref(),
        &projection,
        &n,
    );
    let fetched = {
        let mut tx = conn.begin().await?;
        set_search_options(&mut tx).await?;
        match qb.build().fetch_all(&mut *tx).await {
            Ok(rows) => {
                tx.commit().await?;
                Ok(rows)
            }
            // Dropping the transaction rolls it back.
            Err(e) => Err(e),
        }
    };
    let rows = match fetched {
        Ok(rows) => rows,
        Err(e) => return Err(explain_failure(conn, e).await),
    };
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let mut row = projection.row(coll, r.try_get("row")?);
        let d: f64 = r.try_get("_distance")?;
        row["_distance"] = if d.is_finite() { json!(d) } else { Value::Null };
        out.push(row);
    }
    Ok(Page {
        rows: out,
        next: None,
        included: None,
    })
}

/// Iterative index scans (pgvector 0.8): filtering after the index must not leave the page short.
/// `strict_order` keeps the results in distance order.
async fn set_search_options(tx: &mut PgConnection) -> Result<()> {
    sqlx::query("SET LOCAL hnsw.iterative_scan = strict_order")
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// A statement that failed: when the cause is that pgvector is gone, say so.
async fn explain_failure(conn: &mut PgConnection, e: sqlx::Error) -> Error {
    let code = e
        .as_database_error()
        .and_then(|d| d.code())
        .map(|c| c.to_string())
        .unwrap_or_default();
    // 42704 undefined object (the type), 42883 undefined function (the operator), 42703 undefined column
    // (the extension was dropped with CASCADE).
    if matches!(code.as_str(), "42704" | "42883" | "42703") && missing_extension(conn).await {
        return Error::Unavailable(
            "nearest needs the pgvector extension, which is not installed in this database".into(),
        );
    }
    e.into()
}

async fn missing_extension(conn: &mut PgConnection) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT NOT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'vector')",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap_or(false)
}

/// The query plan of a `nearest` query, for tests that check the index is used.
pub(crate) async fn explain(
    conn: &mut PgConnection,
    coll: &Collection,
    caller: &Caller,
    q: &Query,
    settings: &[(&str, &str)],
) -> Result<Vec<String>> {
    let n = check(coll, q)?;
    let filter = match &q.filter {
        Some(f) => Some(Filter::parse(coll, f)?),
        None => None,
    };
    let projection = Projection::new(coll, q.select.as_deref(), &[])?;
    let plan = plan_read(conn, coll, caller, q.spaces.as_deref(), q.trashed).await?;
    let mut qb = QueryBuilder::<Postgres>::new("EXPLAIN (COSTS OFF) ");
    push_sql(
        &mut qb,
        coll,
        caller,
        &plan,
        filter.as_ref(),
        &projection,
        &n,
    );
    let mut tx = conn.begin().await?;
    set_search_options(&mut tx).await?;
    for (name, value) in settings {
        // Planner settings of a test; the names are not user input.
        sqlx::query(sqlx::AssertSqlSafe(format!("SET LOCAL {name} = {value}")))
            .execute(&mut *tx)
            .await?;
    }
    let rows = qb.build().fetch_all(&mut *tx).await?;
    tx.rollback().await?;
    Ok(rows
        .iter()
        .map(|r| r.try_get::<String, _>(0))
        .collect::<std::result::Result<_, _>>()?)
}
