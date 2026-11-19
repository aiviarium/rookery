//! Log collections: append-only, high-volume data (`kind: "log"`).
//!
//! A log row has `seq` (a number from a sequence of the collection's own), `id`, its scope, `created_at` and
//! `created_by`, and the declared fields. Rows are inserted and read, never changed or deleted; nothing is written
//! to the change log, the entity and reference indexes, history or the search index, and **an insert never takes
//! the global ordering lock** (`feed.rs`), so a log's write rate is bounded by Postgres alone. Readers that follow a
//! log use [`Data::tail`](super::Data::tail) with the last `seq` they saw.
//!
//! **Partitions.** The table is partitioned by `created_at`, one partition per day or month (`partition`). A
//! partition is created before it is needed: when a collection is applied, when the first insert of a period
//! finds it missing, and by [`Collections::maintain_logs`](super::Collections::maintain_logs), which also drops the
//! partitions that lie wholly beyond `retention` (a dropped partition is not rewritten row by row).
//!
//! **Tailing in order.** `seq` is assigned when the statement runs, not when the transaction commits, so a
//! row with a lower `seq` can commit after one with a higher: a reader that has seen 9 could miss a 7 that
//! was still in flight. Every insert therefore holds a *shared* advisory lock of the collection until it
//! commits, and `tail` takes the same lock exclusively, only to read the sequence's last value: the exclusive
//! request waits for the inserts in flight, and the inserts that arrive meanwhile wait behind it, so every number
//! up to that value is final (a committed row, or a gap that never fills). The lock is released at once and the
//! rows up to that *horizon* are read without it, so what `tail` returns has no gaps that later fill in, and
//! inserts wait for a reader only for as long as the inserts in flight take to commit, not for the read.
//! Inserts do not wait for each other.

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::{PgConnection, Postgres, QueryBuilder, Row, Transaction};
use uuid::Uuid;

use super::bulk::{ImportFailure, ImportOutcome};
use super::filter::Filter;
use super::projection::Projection;
use super::query::{MAX_LIMIT, plan_read, push_read_where};
use super::schema::{FieldDef, Partition};
use super::values::{Bind, SqlBind, input, row_out};
use super::write::{canonical, checks_access, require, write_scope};
use super::{Collection, Collections, constraints, container_uri};
use crate::Store;
use crate::caller::Caller;
use crate::error::{Error, Result};
use crate::util::{new_id, quote_ident};

// ── time and names: pure ─────────────────────────────────────────────────────────────────────────

/// The partition size of a collection: `partition`, or a day.
pub fn partition_size(coll: &Collection) -> Partition {
    coll.def.partition.unwrap_or(Partition::Day)
}

/// The period `ts` falls in: from its start (inclusive) to the next one's.
pub fn period(size: Partition, ts: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    match size {
        Partition::Day => {
            let start = Utc
                .with_ymd_and_hms(ts.year(), ts.month(), ts.day(), 0, 0, 0)
                .single()
                .expect("a date");
            (start, start + Duration::days(1))
        }
        Partition::Month => {
            let start = Utc
                .with_ymd_and_hms(ts.year(), ts.month(), 1, 0, 0, 0)
                .single()
                .expect("a date");
            let (y, m) = if ts.month() == 12 {
                (ts.year() + 1, 1)
            } else {
                (ts.year(), ts.month() + 1)
            };
            (
                start,
                Utc.with_ymd_and_hms(y, m, 1, 0, 0, 0)
                    .single()
                    .expect("a date"),
            )
        }
    }
}

/// The table name of the partition of collection `name` that starts at `start`: `<name>__p20261117`, or
/// `<name>__p202611` for a month.
pub fn partition_name(name: &str, size: Partition, start: DateTime<Utc>) -> String {
    match size {
        Partition::Day => format!("{name}__p{}", start.format("%Y%m%d")),
        Partition::Month => format!("{name}__p{}", start.format("%Y%m")),
    }
}

/// The start of the period a partition table name stands for, if it is one of collection `name` of that size.
pub fn partition_start(name: &str, size: Partition, relname: &str) -> Option<DateTime<Utc>> {
    let digits = relname.strip_prefix(name)?.strip_prefix("__p")?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match (size, digits.len()) {
        (Partition::Day, 8) => Utc
            .with_ymd_and_hms(
                digits[..4].parse().ok()?,
                digits[4..6].parse().ok()?,
                digits[6..].parse().ok()?,
                0,
                0,
                0,
            )
            .single(),
        (Partition::Month, 6) => Utc
            .with_ymd_and_hms(
                digits[..4].parse().ok()?,
                digits[4..].parse().ok()?,
                1,
                0,
                0,
                0,
            )
            .single(),
        _ => None,
    }
}

/// Whether the partition starting at `start` is wholly older than `retention_days` at `now`: its last moment is
/// before the cutoff, so no row in it is younger than the retention.
pub fn expired(
    size: Partition,
    start: DateTime<Utc>,
    now: DateTime<Utc>,
    retention_days: u32,
) -> bool {
    let (_, end) = period(size, start);
    end <= now - Duration::days(i64::from(retention_days))
}

fn sql_time(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S+00").to_string()
}

// ── partitions ───────────────────────────────────────────────────────────────────────────────────

/// Creates the partitions of the period of `now` and the next one, where they do not exist.
pub(crate) async fn create_partitions(
    conn: &mut PgConnection,
    coll: &Collection,
    now: DateTime<Utc>,
) -> Result<Vec<String>> {
    let size = partition_size(coll);
    let (start, end) = period(size, now);
    let (next_start, next_end) = period(size, end);
    // Serialised, so that two inserts that both find the partition missing do not both wait on the table's
    // exclusive lock to create it.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('rookery.logpart:' || $1))")
        .bind(&coll.table)
        .execute(&mut *conn)
        .await?;
    let mut created = Vec::new();
    for (from, to) in [(start, end), (next_start, next_end)] {
        let name = partition_name(&coll.def.name, size, from);
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(format!(
                "{}.{}",
                quote_ident(&coll.schema),
                quote_ident(&name)
            ))
            .fetch_one(&mut *conn)
            .await?;
        if exists {
            continue;
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE TABLE {}.{} PARTITION OF {} FOR VALUES FROM ('{}') TO ('{}')",
            quote_ident(&coll.schema),
            quote_ident(&name),
            coll.table,
            sql_time(from),
            sql_time(to)
        )))
        .execute(&mut *conn)
        .await?;
        created.push(name);
    }
    Ok(created)
}

/// Makes sure the partitions an insert now needs exist, asking the database only when the period changed.
async fn ensure_partitions(store: &Store, coll: &Collection, force: bool) -> Result<()> {
    let mut now = Utc::now();
    if !force
        && store
            .data
            .log_partitions
            .lock()
            .expect("log partitions")
            .get(&coll.table)
            .is_some_and(|until| now < *until)
    {
        return Ok(());
    }
    if force {
        // The database's clock decides which partition a row lands in.
        now = sqlx::query_scalar("SELECT now()")
            .fetch_one(store.pool())
            .await?;
    }
    let mut tx = store.pool().begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '10s'")
        .execute(&mut *tx)
        .await?;
    create_partitions(&mut tx, coll, now).await?;
    tx.commit().await?;
    let (_, end) = period(partition_size(coll), now);
    store
        .data
        .log_partitions
        .lock()
        .expect("log partitions")
        .insert(coll.table.clone(), end);
    Ok(())
}

fn no_partition(e: &Error) -> bool {
    matches!(e, Error::Database(sqlx::Error::Database(d)) if d.code().as_deref() == Some("23514"))
}

/// What [`Collections::maintain_logs`] did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaintainReport {
    pub created: Vec<String>,
    pub dropped: Vec<String>,
}

impl Collections<'_> {
    /// For every log collection: creates the partitions for `now` and the next period, and drops the partitions
    /// that are wholly older than the collection's `retention`. Run on a schedule (the kernel does, hourly).
    pub async fn maintain_logs(&self, now: DateTime<Utc>) -> Result<MaintainReport> {
        let mut report = MaintainReport::default();
        let logs: Vec<_> = {
            let registry = self.store.data.registry.read().expect("registry lock");
            registry
                .values()
                .filter(|c| c.def.is_log())
                .cloned()
                .collect()
        };
        for coll in logs {
            let mut tx = self.store.pool().begin().await?;
            sqlx::query("SET LOCAL lock_timeout = '10s'")
                .execute(&mut *tx)
                .await?;
            report
                .created
                .extend(create_partitions(&mut tx, &coll, now).await?);
            let Some(retention) = coll.def.retention else {
                tx.commit().await?;
                continue;
            };
            let size = partition_size(&coll);
            let existing: Vec<String> = sqlx::query_scalar(
                "SELECT c.relname::text FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid \
                 WHERE i.inhparent = $1::regclass",
            )
            .bind(&coll.table)
            .fetch_all(&mut *tx)
            .await?;
            for relname in existing {
                let Some(start) = partition_start(&coll.def.name, size, &relname) else {
                    continue;
                };
                if expired(size, start, now, retention.days) {
                    sqlx::query(sqlx::AssertSqlSafe(format!(
                        "DROP TABLE {}.{}",
                        quote_ident(&coll.schema),
                        quote_ident(&relname)
                    )))
                    .execute(&mut *tx)
                    .await?;
                    report.dropped.push(relname);
                }
            }
            tx.commit().await?;
        }
        Ok(report)
    }
}

// ── writes ───────────────────────────────────────────────────────────────────────────────────────

/// A row that has passed the checks and is ready to be written.
struct Prepared<'a> {
    id: Uuid,
    cols: Vec<(&'a FieldDef, Bind)>,
}

fn prepare<'a>(coll: &'a Collection, values: &Map<String, Value>) -> Result<Prepared<'a>> {
    let mut id = new_id();
    let mut cols: Vec<(&FieldDef, Bind)> = Vec::new();
    let mut given: Map<String, Value> = Map::new();
    for (k, v) in values {
        if k == "id" {
            id = v
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| Error::invalid("id must be a UUID"))?;
            continue;
        }
        let f = coll
            .def
            .field(k)
            .ok_or_else(|| Error::invalid(format!("unknown field {k}")))?;
        let b = input(&coll.def.name, f, v)?;
        let val = canonical(f, &b, v);
        constraints::check_field_value(&coll.def.name, f, &val).map_err(Error::from)?;
        given.insert(k.clone(), val);
        cols.push((f, b));
    }
    for f in &coll.def.fields {
        if given.contains_key(&f.name) {
            continue;
        }
        if let Some(d) = &f.default {
            let b = input(&coll.def.name, f, d)?;
            let val = canonical(f, &b, d);
            constraints::check_field_value(&coll.def.name, f, &val).map_err(Error::from)?;
            cols.push((f, b));
        } else {
            constraints::check_field_value(&coll.def.name, f, &Value::Null).map_err(Error::from)?;
        }
    }
    Ok(Prepared { id, cols })
}

/// The shared lock every insert holds until it commits, and `tail` takes exclusively.
fn log_lock_key(coll: &Collection) -> String {
    format!("rookery.log:{}.{}", coll.plugin, coll.def.name)
}

async fn lock_shared(tx: &mut Transaction<'static, Postgres>, coll: &Collection) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtext($1))")
        .bind(log_lock_key(coll))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn check_write_access(
    tx: &mut Transaction<'static, Postgres>,
    coll: &Collection,
    caller: &Caller,
) -> Result<()> {
    let scope = write_scope(coll, caller)?;
    if checks_access(coll, caller) {
        require(
            tx,
            caller,
            &container_uri(coll, scope.space, scope.user),
            coll.def.access.write,
        )
        .await?;
    }
    Ok(())
}

/// Inserts rows with one statement per as many as the parameter limit allows. The rows come back in order.
async fn insert_rows(
    tx: &mut Transaction<'static, Postgres>,
    coll: &Collection,
    caller: &Caller,
    rows: Vec<Prepared<'_>>,
) -> Result<Vec<Value>> {
    let scope = write_scope(coll, caller)?;
    let per_statement = (60_000 / (coll.def.fields.len() + 5)).max(1);
    let mut out = Vec::with_capacity(rows.len());
    let mut rows = rows.into_iter().peekable();
    while rows.peek().is_some() {
        let mut qb = QueryBuilder::<Postgres>::new(format!(
            "INSERT INTO {} AS t (id, space_id, user_id, created_by",
            coll.table
        ));
        for f in &coll.def.fields {
            qb.push(", ");
            qb.push(quote_ident(&f.name));
        }
        qb.push(") VALUES ");
        for (i, row) in rows.by_ref().take(per_statement).enumerate() {
            if i > 0 {
                qb.push(", ");
            }
            qb.push("(");
            qb.push_bind(row.id);
            qb.push(", ");
            qb.push_bind(scope.space);
            qb.push(", ");
            qb.push_bind(scope.user);
            qb.push(", ");
            qb.push_bind(caller.user);
            let mut cols = row.cols;
            for f in &coll.def.fields {
                qb.push(", ");
                let b = match cols.iter().position(|(c, _)| c.name == f.name) {
                    Some(i) => cols.swap_remove(i).1,
                    None => Bind::Null,
                };
                b.push(&mut qb, &f.ty.sql_type());
            }
            qb.push(")");
        }
        qb.push(" RETURNING to_jsonb(t) AS row");
        for r in qb.build().fetch_all(&mut **tx).await? {
            out.push(row_out(coll, r.try_get("row")?));
        }
    }
    Ok(out)
}

/// Appends one row.
pub(crate) async fn insert(
    store: &Store,
    coll: &Collection,
    caller: &Caller,
    values: &Map<String, Value>,
    idempotency: Option<&crate::idempotency::Key>,
) -> Result<Value> {
    let mut attempts = 0;
    ensure_partitions(store, coll, false).await?;
    loop {
        attempts += 1;
        let prepared = prepare(coll, values)?;
        let mut tx = store.pool().begin().await?;
        lock_shared(&mut tx, coll).await?;
        check_write_access(&mut tx, coll, caller).await?;
        match insert_rows(&mut tx, coll, caller, vec![prepared]).await {
            Ok(mut rows) => {
                let row = rows.remove(0);
                if let Some(key) = idempotency
                    && !crate::idempotency::record(
                        &mut tx,
                        key,
                        &crate::idempotency::rows_response(std::slice::from_ref(&Some(
                            row.clone(),
                        ))),
                    )
                    .await?
                {
                    drop(tx);
                    return replay_insert(store, key).await;
                }
                tx.commit().await?;
                return Ok(row);
            }
            Err(e) if attempts == 1 && no_partition(&e) => {
                drop(tx);
                ensure_partitions(store, coll, true).await?;
            }
            Err(e) => return Err(e),
        }
    }
}

/// The row recorded for an insert with this idempotency key.
pub(crate) async fn replay_insert(store: &Store, key: &crate::idempotency::Key) -> Result<Value> {
    let mut conn = store.pool().acquire().await?;
    crate::idempotency::stored(&mut conn, key)
        .await?
        .and_then(|r| crate::idempotency::rows_of(&r).into_iter().next().flatten())
        .ok_or_else(|| Error::conflict("a write with this idempotency key is in progress"))
}

/// Appends up to 500 rows in one transaction. A row that fails its checks is reported by index and the others are
/// written; any other failure writes nothing.
pub(crate) async fn import(
    store: &Store,
    coll: &Collection,
    caller: &Caller,
    rows: &[Value],
    idempotency: Option<&crate::idempotency::Key>,
) -> Result<ImportOutcome> {
    let mut out = ImportOutcome::default();
    let mut ready = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        match row
            .as_object()
            .ok_or_else(|| Error::invalid("a row must be an object"))
            .and_then(|v| prepare(coll, v))
        {
            Ok(p) => ready.push(p),
            Err(e) => out.failed.push(ImportFailure {
                index,
                code: e.code().as_str().to_string(),
                message: e.to_string(),
                details: e.details(),
            }),
        }
    }
    out.inserted = ready.len() as u64;
    ensure_partitions(store, coll, false).await?;
    let response = serde_json::to_value(&out).map_err(|e| Error::internal(e.to_string()))?;
    let mut attempts = 0;
    loop {
        attempts += 1;
        // The rows are prepared again on a retry: a prepared row is consumed by the statement.
        let batch: Vec<Prepared<'_>> = if attempts == 1 {
            std::mem::take(&mut ready)
        } else {
            rows.iter()
                .filter_map(|r| r.as_object().and_then(|v| prepare(coll, v).ok()))
                .collect()
        };
        let mut tx = store.pool().begin().await?;
        lock_shared(&mut tx, coll).await?;
        check_write_access(&mut tx, coll, caller).await?;
        match insert_rows(&mut tx, coll, caller, batch).await {
            Ok(_) => {
                if let Some(key) = idempotency
                    && !crate::idempotency::record(&mut tx, key, &response).await?
                {
                    drop(tx);
                    return super::bulk::replay(store, key).await?.ok_or_else(|| {
                        Error::conflict("a write with this idempotency key is in progress")
                    });
                }
                tx.commit().await?;
                return Ok(out);
            }
            Err(e) if attempts == 1 && no_partition(&e) => {
                drop(tx);
                ensure_partitions(store, coll, true).await?;
            }
            Err(e) => return Err(e),
        }
    }
}

// ── reading in order ─────────────────────────────────────────────────────────────────────────────

/// `tail`: the rows after a `seq`, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TailQuery {
    /// Rows with a larger `seq`; 0 reads from the start.
    #[serde(default)]
    pub from_seq: i64,
    #[serde(default, rename = "where", skip_serializing_if = "Option::is_none")]
    pub filter: Option<Value>,
    /// At most this many (default and most 1,000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spaces: Option<Vec<Uuid>>,
}

/// What `tail` returns: the rows, and the `seq` to continue from (the last row's, or `fromSeq` when there are
/// none).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tail {
    pub rows: Vec<Value>,
    pub seq: i64,
}

/// The highest `seq` that is final: every number up to it belongs to a committed row or to an insert that rolled back.
/// Taking the ordering lock exclusively waits for the inserts in flight and keeps new ones out, so, while it is
/// held, the sequence's last value is such a number; the lock is released again at once, and the rows are read
/// without it (an insert that starts meanwhile draws a higher number than the horizon).
async fn horizon(store: &Store, coll: &Collection) -> Result<i64> {
    let mut tx = store.pool().begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(log_lock_key(coll))
        .execute(&mut *tx)
        .await?;
    let last: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT CASE WHEN is_called THEN last_value ELSE 0 END FROM {}",
        super::ddl::log_sequence(coll)
    )))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(last)
}

pub(crate) async fn tail(
    store: &Store,
    coll: &Collection,
    caller: &Caller,
    q: &TailQuery,
) -> Result<Tail> {
    let limit = q.limit.unwrap_or(MAX_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(Error::invalid(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }
    if q.from_seq < 0 {
        return Err(Error::invalid("fromSeq must not be negative"));
    }
    let filter = match &q.filter {
        Some(f) => Some(Filter::parse(coll, f)?),
        None => None,
    };
    let projection = Projection::new(coll, q.select.as_deref(), &[])?;
    let horizon = horizon(store, coll).await?;
    let mut tx = store.pool().begin().await?;
    let plan = plan_read(&mut tx, coll, caller, q.spaces.as_deref(), false).await?;
    let mut qb = QueryBuilder::<Postgres>::new("SELECT ");
    projection.push(&mut qb);
    qb.push(format!(" AS row FROM {} t", coll.table));
    push_read_where(&mut qb, coll, caller, &plan);
    qb.push(" AND t.seq > ");
    qb.push_bind(q.from_seq);
    qb.push(" AND t.seq <= ");
    qb.push_bind(horizon);
    if let Some(f) = &filter {
        qb.push(" AND ");
        f.push_sql(coll, &mut qb);
    }
    qb.push(" ORDER BY t.seq LIMIT ");
    qb.push_bind(limit);
    let fetched = qb.build().fetch_all(&mut *tx).await?;
    tx.commit().await?;
    let mut rows = Vec::with_capacity(fetched.len());
    for r in &fetched {
        rows.push(projection.row(coll, r.try_get("row")?));
    }
    let seq = rows
        .last()
        .and_then(|r| r.get("seq"))
        .and_then(Value::as_i64)
        .unwrap_or(q.from_seq);
    Ok(Tail { rows, seq })
}
