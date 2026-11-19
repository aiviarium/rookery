//! Reads: `find`, `get`, `count`. SQL is generated from the structured query with every value
//! bound as a parameter; names come from the validated collection definition.

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgConnection, Postgres, QueryBuilder, Row};
use uuid::Uuid;

use super::filter::Filter;
use super::projection::Projection;
use super::schema::{FieldType, Scope};
use super::values::{Kind, SqlBind, SqlKind, row_out, to_bind};
use super::{Collection, container_uri};
use crate::caller::{Caller, Level};
use crate::entities::prefix_tsquery;
use crate::error::{Error, Result};
use crate::util::quote_ident;

pub const DEFAULT_LIMIT: i64 = 50;
pub const MAX_LIMIT: i64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Asc,
    Desc,
}

/// One `orderBy` entry: `"field"` or `["field", "desc"]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OrderBy {
    Field(String),
    Pair(String, Direction),
}

impl OrderBy {
    fn parts(&self) -> (&str, Direction) {
        match self {
            OrderBy::Field(f) => (f, Direction::Asc),
            OrderBy::Pair(f, d) => (f, *d),
        }
    }
}

/// A structured query.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Query {
    /// The filter; see `packages/crow-filter/FILTER.md`.
    #[serde(default, rename = "where", skip_serializing_if = "Option::is_none")]
    pub filter: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_by: Option<Vec<OrderBy>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// The `next` cursor of the previous page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Full-text search over the fields marked `search`. Without `orderBy`, results are
    /// ordered by rank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    /// Space-scoped collections: query these spaces instead of the caller's current one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spaces: Option<Vec<Uuid>>,
    /// Only rows in the trash.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trashed: bool,
    /// The declared fields to return. Without it, every field but `bytes` and `vector` ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<Vec<String>>,
    /// Vector search; replaces `orderBy`, `after` and `search`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nearest: Option<super::nearest::Nearest>,
    /// `ref` and `refs` fields whose rows are returned beside the page, in `included`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
}

impl Query {
    pub fn filter(filter: Value) -> Self {
        Query {
            filter: Some(filter),
            ..Default::default()
        }
    }
}

/// A page of rows and the cursor of the next page, if there is one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub rows: Vec<Value>,
    pub next: Option<String>,
    /// With `include`: the referenced rows the caller may read, by the reference's value (an entity URI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub included: Option<serde_json::Map<String, Value>>,
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    k: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    r: Option<f32>,
}

struct OrderCol {
    api: String,
    sql: String,
    kind: Kind,
    desc: bool,
}

/// The rows a read may see: scope, trash state and access.
pub(crate) struct ReadPlan {
    scope_sql: ScopeSql,
    /// None: no per-row check. Some(level): rows must pass `rookery.effective_level`.
    row_check: Option<Level>,
    /// Some(predicate): rows matching this SQL inherit the container's level, which suffices.
    inherits: Option<String>,
    /// A typed table of Rookery's: rows need the level on their `_resource`, or the caller is their `_owner`.
    typed_check: Option<Level>,
    trashed: bool,
}

enum ScopeSql {
    All,
    Spaces(Vec<Uuid>),
    User(Uuid),
}

/// Works out scope and access for a read. Non-entity collections check the container here;
/// entity collections check rows in SQL unless the container's level already covers them.
pub(crate) async fn plan_read(
    conn: &mut PgConnection,
    coll: &Collection,
    caller: &Caller,
    spaces: Option<&[Uuid]>,
    trashed: bool,
) -> Result<ReadPlan> {
    let need = coll.def.access.read;
    coll.def.access.check_admin(&coll.def.name, caller)?;
    let scope_sql = match coll.def.scope {
        Scope::Global => ScopeSql::All,
        Scope::Space => match spaces {
            Some(s) if !s.is_empty() => ScopeSql::Spaces(s.to_vec()),
            Some(_) => return Err(Error::invalid("spaces is empty")),
            None => ScopeSql::Spaces(vec![caller.require_space()?]),
        },
        Scope::User => ScopeSql::User(caller.require_user()?),
    };
    if trashed && !coll.def.trash {
        return Err(Error::invalid(format!(
            "collection {} has no trash",
            coll.def.name
        )));
    }
    let mut plan = ReadPlan {
        scope_sql,
        row_check: None,
        inherits: None,
        typed_check: None,
        trashed,
    };
    if caller.admin || coll.def.scope == Scope::User {
        return Ok(plan);
    }
    if coll.is_typed() {
        // Its rows are checked one by one: on the resource each names, such as a job's subject.
        plan.typed_check = Some(need);
        return Ok(plan);
    }
    let containers: Vec<String> = match &plan.scope_sql {
        ScopeSql::Spaces(s) => s.iter().map(|id| crate::uri::space_uri(*id)).collect(),
        _ => vec![container_uri(coll, None, None)],
    };
    let mut all_ok = true;
    for c in &containers {
        let ok = crate::access::level(conn, caller, c)
            .await?
            .is_some_and(|l| l >= need);
        if !ok {
            if coll.def.entity.is_none() {
                return Err(Error::forbidden(format!(
                    "{need} access to {c} is required"
                )));
            }
            all_ok = false;
        }
    }
    if let Some(e) = &coll.def.entity {
        // A row inherits its container's level when it has no other parent and is not private.
        let mut conds = Vec::new();
        if let Some(p) = &e.parent {
            conds.push(format!("t.{} IS NULL", quote_ident(p)));
        }
        if let Some(p) = &e.private {
            conds.push(format!("NOT coalesce(t.{}, false)", quote_ident(p)));
        }
        if !(all_ok && conds.is_empty()) {
            plan.row_check = Some(need);
            if all_ok {
                plan.inherits = Some(conds.join(" AND "));
            }
        }
    }
    Ok(plan)
}

/// Pushes ` WHERE <scope, trash and access>`.
pub(crate) fn push_read_where(
    qb: &mut QueryBuilder<Postgres>,
    coll: &Collection,
    caller: &Caller,
    plan: &ReadPlan,
) {
    qb.push(" WHERE ");
    match &plan.scope_sql {
        ScopeSql::All => {
            qb.push("TRUE");
        }
        ScopeSql::Spaces(s) if s.len() == 1 => {
            qb.push("t.space_id = ");
            qb.push_bind(s[0]);
        }
        ScopeSql::Spaces(s) => {
            qb.push("t.space_id = ANY(");
            qb.push_bind(s.clone());
            qb.push(")");
        }
        ScopeSql::User(u) => {
            qb.push("t.user_id = ");
            qb.push_bind(*u);
        }
    }
    if !coll.def.is_log() {
        qb.push(if plan.trashed {
            " AND t.deleted_at IS NOT NULL"
        } else {
            " AND t.deleted_at IS NULL"
        });
    }
    if let Some(level) = plan.typed_check {
        qb.push(" AND (t._owner IS NOT DISTINCT FROM ");
        qb.push_bind(caller.user);
        qb.push("::uuid AND t._owner IS NOT NULL OR (t._resource IS NOT NULL AND rookery.effective_level(t._resource, ");
        qb.push_bind(caller.user);
        qb.push("::uuid, (SELECT rookery.principals_of(");
        qb.push_bind(caller.user);
        qb.push("::uuid, ");
        qb.push_bind(caller.actor());
        qb.push("))) >= ");
        qb.push_bind(level.as_i16());
        qb.push("))");
    }
    if let (Some(level), Some(e)) = (plan.row_check, &coll.def.entity) {
        if plan.inherits.is_none() {
            // Without the container's level, only rows reachable from the caller's grants and
            // ownership can be visible; narrow to those before checking each.
            // ARRAY(...) is computed once up front and drives an index scan on the ids.
            let prefix = format!("{}://", e.scheme);
            qb.push(" AND t.id = ANY(ARRAY(SELECT substr(r, ");
            qb.push_bind(prefix.len() as i32 + 1);
            qb.push(")::uuid FROM rookery.reachable(");
            qb.push_bind(caller.user);
            qb.push("::uuid, rookery.principals_of(");
            qb.push_bind(caller.user);
            qb.push("::uuid, ");
            qb.push_bind(caller.actor());
            qb.push(")) AS r WHERE starts_with(r, ");
            qb.push_bind(prefix);
            qb.push(")))");
        }
        qb.push(" AND (");
        if let Some(inh) = &plan.inherits {
            qb.push(format!("({inh}) OR "));
        }
        qb.push("rookery.effective_level(");
        qb.push_bind(format!("{}://", e.scheme));
        qb.push(" || t.id::text, ");
        qb.push_bind(caller.user);
        qb.push("::uuid, (SELECT rookery.principals_of(");
        qb.push_bind(caller.user);
        qb.push("::uuid, ");
        qb.push_bind(caller.actor());
        qb.push("))) >= ");
        qb.push_bind(level.as_i16());
        qb.push(")");
    }
}

fn order_cols(coll: &Collection, order: &[OrderBy]) -> Result<Vec<OrderCol>> {
    let mut out = Vec::new();
    for o in order {
        let (name, dir) = o.parts();
        if name == "id" {
            continue;
        }
        let (sql, kind) = coll
            .column(name)
            .ok_or_else(|| Error::invalid(format!("cannot order by unknown field {name}")))?;
        if matches!(
            kind,
            Kind::Json | Kind::Richtext | Kind::Refs(_) | Kind::Bytes | Kind::Vector
        ) {
            return Err(Error::invalid(format!("cannot order by {name}")));
        }
        out.push(OrderCol {
            api: name.to_string(),
            sql,
            kind,
            desc: dir == Direction::Desc,
        });
    }
    if out.len() > 4 {
        return Err(Error::invalid("order by at most 4 fields"));
    }
    // The last key makes the order total, and the cursor with it: a log's own sequence number, otherwise the id.
    out.push(if coll.def.is_log() {
        OrderCol {
            api: "seq".into(),
            sql: "t.seq".into(),
            kind: Kind::Integer,
            desc: false,
        }
    } else {
        OrderCol {
            api: "id".into(),
            sql: "t.id".into(),
            kind: Kind::Uuid,
            desc: false,
        }
    });
    Ok(out)
}

fn rank_sql(qb: &mut QueryBuilder<Postgres>, tsq: &str) {
    qb.push("ts_rank(t._search, to_tsquery('simple', ");
    qb.push_bind(tsq.to_string());
    qb.push("))");
}

/// Keyset condition "after the cursor" for the order, with Postgres's default null placement
/// (nulls last ascending, first descending).
fn push_after(
    qb: &mut QueryBuilder<Postgres>,
    cols: &[OrderCol],
    cursor: &Cursor,
    rank: Option<&str>,
) -> Result<()> {
    if cursor.k.len() != cols.len() {
        return Err(Error::invalid("the cursor does not belong to this query"));
    }
    let binds = cols
        .iter()
        .zip(&cursor.k)
        .map(|(c, v)| to_bind(&c.kind, v).map_err(|_| Error::invalid("bad cursor")))
        .collect::<Result<Vec<_>>>()?;
    qb.push(" AND (");
    let mut first_disjunct = true;
    let mut disjunct = |qb: &mut QueryBuilder<Postgres>| {
        if !first_disjunct {
            qb.push(" OR ");
        }
        first_disjunct = false;
    };
    // With search ordering, rank is the leading key (descending, never null).
    let rank_prefix = |qb: &mut QueryBuilder<Postgres>, op: &str| -> Result<()> {
        if let (Some(tsq), Some(r)) = (rank, cursor.r) {
            rank_sql(qb, tsq);
            qb.push(format!(" {op} "));
            qb.push_bind(r);
            qb.push("::real");
            Ok(())
        } else if rank.is_some() {
            Err(Error::invalid("the cursor does not belong to this query"))
        } else {
            Ok(())
        }
    };
    if rank.is_some() {
        disjunct(qb);
        qb.push("(");
        rank_prefix(qb, "<")?;
        qb.push(")");
    }
    for k in 0..cols.len() {
        let c = &cols[k];
        let b = &binds[k];
        // Nothing sorts after null ascending: skip that disjunct.
        if !c.desc && b.is_null() {
            continue;
        }
        disjunct(qb);
        qb.push("(");
        let mut and = false;
        if rank.is_some() {
            rank_prefix(qb, "=")?;
            and = true;
        }
        for j in 0..k {
            if and {
                qb.push(" AND ");
            }
            and = true;
            let (cj, bj) = (&cols[j], &binds[j]);
            if bj.is_null() {
                qb.push(format!("{} IS NULL", cj.sql));
            } else {
                qb.push(format!("{} = ", cj.sql));
                bj.clone().push(qb, cj.kind.sql_type());
            }
        }
        if and {
            qb.push(" AND ");
        }
        match (c.desc, b.is_null()) {
            (false, _) => {
                qb.push(format!("({} > ", c.sql));
                b.clone().push(qb, c.kind.sql_type());
                qb.push(format!(" OR {} IS NULL)", c.sql));
            }
            (true, true) => {
                qb.push(format!("{} IS NOT NULL", c.sql));
            }
            (true, false) => {
                qb.push(format!("{} < ", c.sql));
                b.clone().push(qb, c.kind.sql_type());
            }
        }
        qb.push(")");
    }
    if first_disjunct {
        qb.push("FALSE");
    }
    qb.push(")");
    Ok(())
}

fn push_search(qb: &mut QueryBuilder<Postgres>, tsq: &str) {
    qb.push(" AND t._search @@ to_tsquery('simple', ");
    qb.push_bind(tsq.to_string());
    qb.push(")");
}

pub(crate) async fn find(
    conn: &mut PgConnection,
    coll: &Collection,
    caller: &Caller,
    q: &Query,
) -> Result<Page> {
    if q.nearest.is_some() {
        return super::nearest::find(conn, coll, caller, q).await;
    }
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(Error::invalid(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }
    let filter = match &q.filter {
        Some(f) => Some(Filter::parse(coll, f)?),
        None => None,
    };
    let tsq = match &q.search {
        Some(s) => match prefix_tsquery(s) {
            Some(t) => Some(t),
            None => {
                return Ok(Page {
                    rows: Vec::new(),
                    next: None,
                    included: None,
                });
            }
        },
        None => None,
    };
    let order = order_cols(coll, q.order_by.as_deref().unwrap_or(&[]))?;
    let by_rank = tsq.is_some() && q.order_by.as_ref().is_none_or(|o| o.is_empty());
    let cursor: Option<Cursor> = match &q.after {
        Some(a) => Some(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(a)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .ok_or_else(|| Error::invalid("bad cursor"))?,
        ),
        None => None,
    };
    // The cursor is built from the last row's order columns, so they are read even when not selected.
    let order_fields: Vec<String> = order.iter().map(|c| c.api.clone()).collect();
    let projection = Projection::new(coll, q.select.as_deref(), &order_fields)?;
    let plan = plan_read(conn, coll, caller, q.spaces.as_deref(), q.trashed).await?;

    let mut qb = QueryBuilder::<Postgres>::new("SELECT ");
    projection.push(&mut qb);
    qb.push(" AS row");
    if by_rank {
        qb.push(", ");
        rank_sql(&mut qb, tsq.as_deref().expect("search"));
        qb.push(" AS _rank");
    }
    qb.push(format!(" FROM {} t", coll.table));
    push_read_where(&mut qb, coll, caller, &plan);
    if let Some(f) = &filter {
        qb.push(" AND ");
        f.push_sql(coll, &mut qb);
    }
    if let Some(t) = &tsq {
        push_search(&mut qb, t);
    }
    if let Some(c) = &cursor {
        push_after(
            &mut qb,
            &order,
            c,
            if by_rank { tsq.as_deref() } else { None },
        )?;
    }
    qb.push(" ORDER BY ");
    if by_rank {
        rank_sql(&mut qb, tsq.as_deref().expect("search"));
        qb.push(" DESC, ");
    }
    for (i, c) in order.iter().enumerate() {
        if i > 0 {
            qb.push(", ");
        }
        qb.push(format!("{} {}", c.sql, if c.desc { "DESC" } else { "ASC" }));
    }
    qb.push(" LIMIT ");
    qb.push_bind(limit + 1);

    let rows = qb.build().fetch_all(&mut *conn).await?;
    let mut out = Vec::with_capacity(rows.len());
    let mut ranks = Vec::new();
    for r in &rows {
        let v: Value = r.try_get("row")?;
        out.push(projection.row(coll, v));
        if by_rank {
            ranks.push(r.try_get::<f32, _>("_rank")?);
        }
    }
    let next = if out.len() as i64 > limit {
        out.truncate(limit as usize);
        let last = out.last().expect("non-empty");
        let cur = Cursor {
            k: order
                .iter()
                .map(|c| last.get(&c.api).cloned().unwrap_or(Value::Null))
                .collect(),
            r: if by_rank {
                ranks.get(limit as usize - 1).copied()
            } else {
                None
            },
        };
        Some(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&cur).expect("json")),
        )
    } else {
        None
    };
    for row in &mut out {
        projection.strip_extra(row);
    }
    Ok(Page {
        rows: out,
        next,
        included: None,
    })
}

pub(crate) async fn count(
    conn: &mut PgConnection,
    coll: &Collection,
    caller: &Caller,
    q: &Query,
) -> Result<i64> {
    let filter = match &q.filter {
        Some(f) => Some(Filter::parse(coll, f)?),
        None => None,
    };
    let tsq = match &q.search {
        Some(s) => match prefix_tsquery(s) {
            Some(t) => Some(t),
            None => return Ok(0),
        },
        None => None,
    };
    let plan = plan_read(conn, coll, caller, q.spaces.as_deref(), q.trashed).await?;
    let mut qb = QueryBuilder::<Postgres>::new(format!("SELECT count(*) FROM {} t", coll.table));
    push_read_where(&mut qb, coll, caller, &plan);
    if let Some(f) = &filter {
        qb.push(" AND ");
        f.push_sql(coll, &mut qb);
    }
    if let Some(t) = &tsq {
        push_search(&mut qb, t);
    }
    Ok(qb.build().fetch_one(&mut *conn).await?.try_get(0)?)
}

pub(crate) async fn get(
    conn: &mut PgConnection,
    coll: &Collection,
    caller: &Caller,
    id: Uuid,
    trashed: bool,
) -> Result<Option<Value>> {
    let spaces = caller.space.map(|s| vec![s]);
    let plan = plan_read(conn, coll, caller, spaces.as_deref(), trashed).await?;
    let mut qb = QueryBuilder::<Postgres>::new(format!(
        "SELECT to_jsonb(t) - '_search' AS row FROM {} t",
        coll.table
    ));
    push_read_where(&mut qb, coll, caller, &plan);
    qb.push(" AND t.id = ");
    qb.push_bind(id);
    match qb.build().fetch_optional(&mut *conn).await? {
        Some(r) => Ok(Some(row_out(coll, r.try_get("row")?))),
        None => Ok(None),
    }
}

/// The SQL expression and kind of a field or system column, by API name.
pub(crate) fn column(coll: &Collection, name: &str) -> Option<(String, Kind)> {
    let sys = if coll.def.is_log() {
        match name {
            "id" => Some(("t.id", Kind::Uuid)),
            "seq" => Some(("t.seq", Kind::Integer)),
            "spaceId" if coll.def.scope == Scope::Space => Some(("t.space_id", Kind::Uuid)),
            "userId" if coll.def.scope == Scope::User => Some(("t.user_id", Kind::Uuid)),
            "createdAt" => Some(("t.created_at", Kind::Datetime)),
            "createdBy" => Some(("t.created_by", Kind::Uuid)),
            _ => None,
        }
    } else {
        match name {
            "id" => Some(("t.id", Kind::Uuid)),
            "spaceId" if coll.def.scope == Scope::Space => Some(("t.space_id", Kind::Uuid)),
            "userId" if coll.def.scope == Scope::User => Some(("t.user_id", Kind::Uuid)),
            "createdAt" => Some(("t.created_at", Kind::Datetime)),
            "updatedAt" => Some(("t.updated_at", Kind::Datetime)),
            "deletedAt" if coll.def.trash => Some(("t.deleted_at", Kind::Datetime)),
            "createdBy" => Some(("t.created_by", Kind::Uuid)),
            "updatedBy" => Some(("t.updated_by", Kind::Uuid)),
            "rev" => Some(("t.rev", Kind::Integer)),
            _ => None,
        }
    };
    if let Some((sql, kind)) = sys {
        return Some((sql.to_string(), kind));
    }
    let f = coll.def.field(name)?;
    let _: &FieldType = &f.ty;
    Some((format!("t.{}", quote_ident(&f.name)), Kind::of(&f.ty)))
}
