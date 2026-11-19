//! Running a plugin's SQL (docs/platform-split.md, "Raw SQL / Through the kernel").
//!
//! Every statement runs on a connection that logged in as the plugin's role (see the module docs of
//! [`crate::sql`]), with parameters bound from the types Postgres inferred for the statement. Limits, in
//! addition to the role's own timeouts: a statement is given [`Limits::statement`] by the kernel's own clock (the role may
//! `SET statement_timeout = 0`), `query` returns at most [`MAX_ROWS`] rows, a transaction lives
//! [`Limits::tx_total`] and may sit idle [`Limits::tx_idle`], and a plugin has at most [`MAX_TX`] open.
//!
//! # A transaction's commit and the kernel's other connections
//!
//! A transaction opened with a [`LeaseFence`] or carrying published messages (see [`Sql::tx_publish`]) cannot do
//! all of its commit on the plugin's connection: the lease table and the topic tables are the kernel's, and the
//! plugin's role must not see them. The commit is then two commits, in this order:
//!
//! 1. on a connection of the kernel's own pool, in one transaction: the lease row is locked (`SELECT ... FOR
//!    UPDATE`, expiry rechecked) and every published message is numbered and written under the topic's lock;
//! 2. the plugin's transaction commits;
//! 3. the kernel's transaction commits, which is what makes the messages visible and lets go of the lease row.
//!
//! While step 1's lock is held, nobody can release the lease or take it over (both are writes to that row), so
//! the lease is still the caller's when step 2 commits. What remains is a crash between steps 2 and 3: the
//! plugin's data is then committed and the messages are not (and the lease lock is released with the dead
//! connection). A message is therefore never delivered for a rolled-back transaction, and the only loss is the
//! messages of a transaction whose commit raced the kernel's own death, which the plugin sees as a failed
//! commit and must republish after checking what committed. If the kernel dies between steps 1 and 2 the plugin's
//! connection dies with it and Postgres rolls the plugin's transaction back: nothing of the plugin's, no message.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde_json::Value;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgArguments, PgConnectOptions, PgPoolOptions, PgTypeInfo};
use sqlx::{
    Arguments, Column as _, Either, Executor, PgConnection, PgPool, Postgres, Row, Statement,
    ValueRef,
};
use tokio::sync::Mutex;

use super::Sql;
use super::value;
use crate::error::{Error, Result};
use crate::util::random_token;

/// Rows `query` returns at most.
pub const MAX_ROWS: usize = 10_000;
/// The time limits of raw SQL. The defaults are the specification's; tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// How long a statement may run: the role's `statement_timeout`. The kernel's own clock allows a second
    /// more, in case the plugin's SQL set its timeout to 0, and the watchdog two.
    pub statement: Duration,
    /// How long a transaction of the API lives.
    pub tx_total: Duration,
    /// How long a transaction of the API may wait between statements.
    pub tx_idle: Duration,
    /// The role's `idle_in_transaction_session_timeout`, which the watchdog also holds sessions to.
    pub idle_in_transaction: Duration,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            statement: Duration::from_secs(30),
            tx_total: Duration::from_secs(30),
            tx_idle: Duration::from_secs(10),
            idle_in_transaction: Duration::from_secs(15),
        }
    }
}

impl Limits {
    fn client(&self) -> Duration {
        self.statement + Duration::from_secs(1)
    }
}
/// Open transactions per plugin.
pub const MAX_TX: usize = 4;
/// Connections of one plugin's pool on this kernel.
const POOL_SIZE: u32 = 5;
/// The application name migrations connect under. It only labels the session for an operator reading
/// `pg_stat_activity`: a session sets its own name, so nothing is decided by it (the watchdog goes by the
/// sessions the kernel registered, see [`Sql::register_session`]).
pub const MIGRATION_APP: &str = "rookery-sql-migration";
/// How long a session the kernel did not register may live on a role without `database.direct` before the
/// watchdog ends it: a connection the kernel opened registers itself within milliseconds of connecting, and this
/// is the room for that (and for another kernel's registration) to be visible.
pub const REGISTRATION_GRACE: Duration = Duration::from_secs(2);
const APP: &str = "rookery-sql";

/// A result column: its name and its Postgres type, as the lower case name (`int4`, `text[]`, `bytea`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Column {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

impl Column {
    /// Whether the gRPC surface sends this column's values as native bytes.
    pub fn is_bytes(&self) -> bool {
        self.ty == "bytea"
    }
    pub fn is_vector(&self) -> bool {
        self.ty == "vector"
    }
}

/// What a statement that returned rows did, for the audit log: the command its plan names (`INSERT`, `UPDATE`,
/// `DELETE`, `MERGE`) and the rows the command tag counted. A statement that writes no table has none.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Written {
    pub command: String,
    pub rows: u64,
}

/// What `query` returns.
#[derive(Debug, Clone, PartialEq)]
pub struct Rows {
    pub columns: Vec<Column>,
    /// One object per row.
    pub rows: Vec<Value>,
    /// The write the statement did, if it wrote a table (a `SELECT` has none).
    pub written: Option<Written>,
}

/// A prepared statement with its parameters encoded.
struct Prepared {
    columns: Vec<Column>,
    args: PgArguments,
}

/// A parameter already in its binary form, whose type is the one Postgres inferred.
struct Raw {
    ty: PgTypeInfo,
    bytes: Option<Vec<u8>>,
}

impl sqlx::Type<Postgres> for Raw {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(sqlx::postgres::types::Oid(0))
    }
    fn compatible(_: &PgTypeInfo) -> bool {
        true
    }
}

impl<'q> sqlx::Encode<'q, Postgres> for Raw {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> std::result::Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        Ok(match &self.bytes {
            None => sqlx::encode::IsNull::Yes,
            Some(b) => {
                buf.extend_from_slice(b);
                sqlx::encode::IsNull::No
            }
        })
    }
    fn produces(&self) -> Option<PgTypeInfo> {
        Some(self.ty.clone())
    }
}

/// Parses `sql` on the connection and binds `params` to the types it infers.
async fn prepare(conn: &mut PgConnection, sql: &str, params: &[Value]) -> Result<Prepared> {
    let stmt = conn
        .prepare(sqlx::SqlSafeStr::into_sql_str(sqlx::AssertSqlSafe(
            sql.to_owned(),
        )))
        .await
        .map_err(map_error)?;
    let types: Vec<PgTypeInfo> = match stmt.parameters() {
        Some(sqlx::Either::Left(t)) => t.to_vec(),
        Some(sqlx::Either::Right(n)) => {
            vec![PgTypeInfo::with_oid(sqlx::postgres::types::Oid(0)); n]
        }
        None => Vec::new(),
    };
    if types.len() != params.len() {
        return Err(Error::invalid(format!(
            "the statement takes {} parameters and {} were given",
            types.len(),
            params.len()
        )));
    }
    let mut columns = Vec::new();
    for c in stmt.columns() {
        let ty = c.type_info();
        value::supported(ty)
            .map_err(|why| Error::invalid(format!("column {}: {why}", c.name())))?;
        columns.push(Column {
            name: c.name().to_string(),
            ty: sqlx::TypeInfo::name(ty).to_ascii_lowercase(),
        });
    }
    let mut args = PgArguments::default();
    for (i, (ty, p)) in types.iter().zip(params).enumerate() {
        value::supported(ty)
            .map_err(|why| Error::invalid(format!("parameter ${}: {why}", i + 1)))?;
        let bytes = value::encode(ty, &value::param(p)?)
            .map_err(|e| Error::invalid(format!("parameter ${}: {e}", i + 1)))?;
        args.add(Raw {
            ty: ty.clone(),
            bytes,
        })
        .map_err(|e| Error::internal(format!("binding parameter ${}: {e}", i + 1)))?;
    }
    Ok(Prepared { columns, args })
}

fn decode_row(columns: &[Column], row: &sqlx::postgres::PgRow) -> Result<Value> {
    let mut values = Vec::with_capacity(columns.len());
    for (i, col) in sqlx::Row::columns(row).iter().enumerate() {
        let raw = row
            .try_get_raw(i)
            .map_err(|e| Error::internal(e.to_string()))?;
        values.push(if raw.is_null() {
            Value::Null
        } else {
            let bytes = raw.as_bytes().map_err(|e| Error::internal(e.to_string()))?;
            value::decode(col.type_info(), bytes)?
        });
    }
    let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
    Ok(value::row(&names, values))
}

/// The error a plugin sees for a database error: a Rookery error from the SQLSTATE, with Postgres's own
/// message but none of its detail, and a bare "permission denied" where the message would name an object the
/// plugin cannot see.
pub fn map_error(e: sqlx::Error) -> Error {
    let sqlx::Error::Database(d) = &e else {
        return match e {
            sqlx::Error::PoolTimedOut => {
                Error::Unavailable("no database connection is free; try again".into())
            }
            sqlx::Error::Io(_) | sqlx::Error::PoolClosed | sqlx::Error::WorkerCrashed => {
                Error::Unavailable("the database connection ended".into())
            }
            other => Error::Unavailable(format!("database error: {}", short(&other.to_string()))),
        };
    };
    let code = d.code().map(|c| c.into_owned()).unwrap_or_default();
    let message = short(d.message());
    match code.as_str() {
        "42501" | "3F000" | "28000" => Error::Forbidden("permission denied".into()),
        "57014" => Error::Timeout("the statement was cancelled: it ran past its time limit".into()),
        "25P03" => Error::Timeout("the transaction was idle for too long".into()),
        "55P03" => Error::Timeout("a lock was not available in time".into()),
        "40001" | "40P01" | "23505" | "23503" | "23P01" => Error::Conflict(message),
        "57P01" | "57P02" | "57P03" | "08000" | "08003" | "08006" => {
            Error::Unavailable("the database connection ended".into())
        }
        c if c.starts_with("53") => Error::Unavailable(message),
        "28P01" => Error::Unavailable("the plugin's database credential was refused".into()),
        _ => Error::Invalid(message),
    }
}

/// The first line of a message, at most 300 bytes.
fn short(m: &str) -> String {
    let line = m.lines().next().unwrap_or("");
    crate::util::truncate_str(line, 300).to_string()
}

/// The error of a statement that ran out of time on the kernel's clock.
fn timed_out() -> Error {
    Error::Timeout("the statement did not finish in time".into())
}

// ── pools ────────────────────────────────────────────────────────────────────────────────────────

impl Sql<'_> {
    /// The time limits in force.
    pub fn limits(&self) -> Limits {
        self.store.data.sql.limits
    }

    /// The pool of a plugin whose connections log in as its role (`replica`: on `ROOKERY_DATABASE_READ_URL`).
    pub(crate) async fn pool(&self, plugin: &str, replica: bool) -> Result<PgPool> {
        let key = (plugin.to_string(), replica);
        if let Some(p) = self
            .store
            .data
            .sql
            .pools
            .lock()
            .expect("pools lock")
            .get(&key)
        {
            return Ok(p.clone());
        }
        let (password, _, _) = self.credential(plugin).await?.ok_or_else(|| {
            Error::not_found(format!("plugin {plugin} has no raw SQL (database.sql)"))
        })?;
        let names = self.names(plugin)?;
        let base: PgConnectOptions = if replica {
            self.store
                .data
                .sql
                .read_url
                .as_deref()
                .ok_or_else(|| {
                    Error::invalid("no read replica is configured (ROOKERY_DATABASE_READ_URL)")
                })?
                .parse()?
        } else {
            (*self.store.pool().connect_options()).clone()
        };
        let vector_schema = self.vector_schema().await?;
        let options = role_options(
            base,
            &names,
            &password,
            APP,
            &self.limits(),
            vector_schema.as_deref(),
        );
        // Every connection the kernel opens as the plugin's role is written down (pid and start time) before any
        // statement of the plugin runs on it: the watchdog tells the kernel's own sessions from a session the
        // plugin opened itself by that. The replica is another server, which the watchdog does not reach.
        let kernel_pool = self.store.pool().clone();
        let role = names.role.clone();
        let register = !replica;
        let pool = PgPoolOptions::new()
            .max_connections(POOL_SIZE)
            .after_connect(move |conn, _| {
                let kernel_pool = kernel_pool.clone();
                let role = role.clone();
                Box::pin(async move {
                    if register {
                        register_session(&kernel_pool, conn, &role, "pool").await?;
                    }
                    Ok(())
                })
            })
            .min_connections(0)
            .acquire_timeout(Duration::from_secs(10))
            .idle_timeout(Duration::from_secs(60))
            .max_lifetime(Duration::from_secs(30 * 60))
            .after_release(|conn, _| {
                Box::pin(async move {
                    // Whatever the plugin's SQL changed in the session (settings, temporary tables, advisory
                    // locks, prepared statements, a role it switched to) goes before another statement uses it.
                    // A connection that cannot be reset is closed.
                    Ok(conn.execute("DISCARD ALL").await.is_ok())
                })
            })
            .connect_lazy_with(options);
        let mut pools = self.store.data.sql.pools.lock().expect("pools lock");
        Ok(pools.entry(key).or_insert(pool).clone())
    }

    /// A connection of the plugin's pool. A refused password (the role changed its own, or it was rotated by
    /// hand) is put right once: the credential is set again from the sealed one.
    pub(crate) async fn connection(
        &self,
        plugin: &str,
        replica: bool,
    ) -> Result<PoolConnection<Postgres>> {
        let pool = self.pool(plugin, replica).await?;
        match pool.acquire().await {
            Ok(c) => Ok(c),
            Err(sqlx::Error::Database(d)) if d.code().as_deref() == Some("28P01") && !replica => {
                self.restore_password(plugin).await?;
                self.forget(plugin).await;
                self.pool(plugin, replica)
                    .await?
                    .acquire()
                    .await
                    .map_err(map_error)
            }
            Err(e) => Err(map_error(e)),
        }
    }

    /// Sets the role's password to the sealed one (a role may change its own).
    async fn restore_password(&self, plugin: &str) -> Result<()> {
        let names = self.names(plugin)?;
        let (password, _, _) = self
            .credential(plugin)
            .await?
            .ok_or_else(|| Error::not_found(format!("plugin {plugin} has no raw SQL")))?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER ROLE {} PASSWORD {}",
            super::names::quote_ident(&names.role),
            super::names::quote_literal(&super::scram::verifier(&password))?
        )))
        .execute(self.store.pool())
        .await?;
        Ok(())
    }

    // ── statements ───────────────────────────────────────────────────────────────────────────────

    /// Runs a statement that returns rows. At most [`MAX_ROWS`]; more fails with `INVALID`.
    pub async fn query(
        &self,
        plugin: &str,
        sql: &str,
        params: &[Value],
        replica: bool,
    ) -> Result<Rows> {
        let mut conn = self.connection(plugin, replica).await?;
        let command = self.write_command(&mut conn, sql, params, false).await;
        let out = tokio::time::timeout(
            self.limits().client(),
            collect(&mut conn, sql, params, command),
        )
        .await;
        finish(conn, out)
    }

    /// Runs a statement and returns the number of rows it affected.
    pub async fn execute(&self, plugin: &str, sql: &str, params: &[Value]) -> Result<u64> {
        let mut conn = self.connection(plugin, false).await?;
        let out =
            tokio::time::timeout(self.limits().client(), affected(&mut conn, sql, params)).await;
        finish(conn, out)
    }

    /// Runs a statement that returns rows and streams them, with no cap. The connection is held until the
    /// stream ends or is dropped; the statement has no time limit of its own beyond the role's, since a
    /// stream's reader sets its pace.
    /// The statement's command when its plan names a write (`INSERT`, `UPDATE`, `DELETE`, `MERGE`),
    /// `EXPLAIN`'s answer; the stream is `None`-commanded for a read, and for a statement the planner cannot
    /// explain (the statement still runs, and is not audited).
    pub async fn query_stream(
        &self,
        plugin: &str,
        sql: &str,
        params: &[Value],
        replica: bool,
    ) -> Result<(
        Vec<Column>,
        futures_util::stream::BoxStream<'static, Result<Value>>,
        Option<String>,
    )> {
        let mut conn = self.connection(plugin, replica).await?;
        let written = self.write_command(&mut conn, sql, params, false).await;
        let prepared = prepare(&mut conn, sql, params).await?;
        let columns = prepared.columns.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Value>>(64);
        let sql = sql.to_string();
        let cols = columns.clone();
        tokio::spawn(async move {
            let mut rows = sqlx::query_with(sqlx::AssertSqlSafe(sql.clone()), prepared.args)
                .persistent(false)
                .fetch(&mut *conn);
            while let Some(r) = rows.next().await {
                let item = r.map_err(map_error).and_then(|row| decode_row(&cols, &row));
                let stop = item.is_err();
                if tx.send(item).await.is_err() || stop {
                    // The reader is gone or the statement failed: end the session rather than drain it.
                    drop(rows);
                    conn.close_on_drop();
                    return;
                }
            }
        });
        Ok((
            columns,
            futures_util::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|item| (item, rx))
            })
            .boxed(),
            written,
        ))
    }

    /// The command a statement writes with, from its plan: `EXPLAIN (FORMAT JSON)` names a `ModifyTable` node
    /// `INSERT`, `UPDATE`, `DELETE` or `MERGE`. The answer is cached by statement text: a statement's command
    /// does not change. A statement the planner cannot explain (a utility statement, a table that does not exist)
    /// has none; it is then not audited, and it runs as it would have.
    ///
    /// The query returns rows, so its command tag cannot be read through the driver (sqlx keeps the row count
    /// only); the plan is what tells `insert ... returning` from `select`, without the kernel parsing SQL.
    async fn write_command(
        &self,
        conn: &mut PgConnection,
        sql: &str,
        params: &[Value],
        in_transaction: bool,
    ) -> Option<String> {
        if let Some(command) = self
            .store
            .data
            .sql
            .writes
            .lock()
            .expect("writes lock")
            .get(sql)
        {
            return Some(command.clone());
        }
        // Inside the plugin's transaction, a failed `EXPLAIN` would abort it: the probe runs in a savepoint, so a
        // statement the role may not explain leaves the transaction as it was. Outside one, a failed statement
        // ends its own implicit transaction and nothing else.
        if in_transaction && conn.execute("SAVEPOINT rookery_audit").await.is_err() {
            return None;
        }
        let explain = format!("EXPLAIN (FORMAT JSON) {sql}");
        let probed = async {
            let prepared = prepare(conn, &explain, params).await.ok()?;
            let row = sqlx::query_with(sqlx::AssertSqlSafe(explain.clone()), prepared.args)
                .persistent(false)
                .fetch_one(&mut *conn)
                .await
                .ok()?;
            let plan: Value = row.try_get(0).ok()?;
            modify_command(&plan)
        }
        .await;
        if in_transaction {
            let _ = conn
                .execute(if probed.is_some() {
                    "RELEASE SAVEPOINT rookery_audit"
                } else {
                    "ROLLBACK TO SAVEPOINT rookery_audit"
                })
                .await;
        }
        let command = probed?;
        // Long statements are not kept: the cache is a few hundred texts, not a place for a plugin to park memory.
        if sql.len() <= 4096 {
            let mut writes = self.store.data.sql.writes.lock().expect("writes lock");
            if writes.len() >= 1024 {
                writes.clear();
            }
            writes.insert(sql.to_string(), command.clone());
        }
        Some(command)
    }
}

/// The command of the first `ModifyTable` node in an `EXPLAIN (FORMAT JSON)` plan, upper-cased (`INSERT`,
/// `UPDATE`, `DELETE`, `MERGE`). A data-modifying CTE has one under a `SELECT`'s plans, so it is found there too.
fn modify_command(plan: &Value) -> Option<String> {
    match plan {
        Value::Array(items) => items.iter().find_map(modify_command),
        Value::Object(map) => {
            if map.get("Node Type").and_then(Value::as_str) == Some("ModifyTable")
                && let Some(op) = map.get("Operation").and_then(Value::as_str)
            {
                return Some(op.to_ascii_uppercase());
            }
            map.values().find_map(modify_command)
        }
        _ => None,
    }
}

/// The login options of a plugin's role over `base`: its credentials and the limits it is held to.
pub(crate) fn role_options(
    base: PgConnectOptions,
    names: &super::Names,
    password: &str,
    application: &str,
    limits: &Limits,
    vector_schema: Option<&str>,
) -> PgConnectOptions {
    let opts = base
        .username(&names.role)
        .password(password)
        .application_name(application)
        // No cache: `DISCARD ALL` deallocates what a cache would remember.
        .statement_cache_capacity(0);
    // Startup options outrank the role's own `ALTER ROLE ... SET` defaults, which the role can change.
    let o = opts.options([(
        "search_path",
        // Unquoted names: a startup option is text, and ours are lower case words and underscores.
        match vector_schema {
            Some(v) => format!("{},{},{v}", names.raw_schema, names.view_schema),
            None => format!("{},{}", names.raw_schema, names.view_schema),
        },
    )]);
    o.options([
        (
            "statement_timeout",
            limits.statement.as_millis().to_string(),
        ),
        (
            "idle_in_transaction_session_timeout",
            limits.idle_in_transaction.as_millis().to_string(),
        ),
        ("lock_timeout", "10000".to_string()),
    ])
}

fn finish<T>(
    conn: PoolConnection<Postgres>,
    out: std::result::Result<Result<T>, tokio::time::error::Elapsed>,
) -> Result<T> {
    match out {
        Ok(r) => r,
        Err(_) => {
            // Still running on the server (the watchdog ends it); this session is not reused.
            let mut conn = conn;
            conn.close_on_drop();
            Err(timed_out())
        }
    }
}

async fn collect(
    conn: &mut PgConnection,
    sql: &str,
    params: &[Value],
    command: Option<String>,
) -> Result<Rows> {
    let prepared = prepare(conn, sql, params).await?;
    let columns = prepared.columns.clone();
    let mut stream = (&mut *conn).fetch_many(
        sqlx::query_with(sqlx::AssertSqlSafe(sql.to_owned()), prepared.args).persistent(false),
    );
    let mut rows = Vec::new();
    let mut affected = 0;
    while let Some(r) = stream.next().await {
        match r.map_err(map_error)? {
            Either::Left(result) => affected = result.rows_affected(),
            Either::Right(row) => {
                if rows.len() >= MAX_ROWS {
                    drop(stream);
                    return Err(Error::invalid(format!(
                        "the query returned more than {MAX_ROWS} rows; add a limit or use queryStream"
                    )));
                }
                rows.push(decode_row(&columns, &row)?);
            }
        }
    }
    Ok(Rows {
        columns,
        rows,
        written: command.map(|command| Written {
            command,
            rows: affected,
        }),
    })
}

async fn affected(conn: &mut PgConnection, sql: &str, params: &[Value]) -> Result<u64> {
    let prepared = prepare(conn, sql, params).await?;
    let r = sqlx::query_with(sqlx::AssertSqlSafe(sql.to_owned()), prepared.args)
        .persistent(false)
        .execute(&mut *conn)
        .await
        .map_err(map_error)?;
    Ok(r.rows_affected())
}

// ── transactions ─────────────────────────────────────────────────────────────────────────────────

/// A lease a transaction commits only while the caller still holds it (docs/platform-split.md, "Leases /
/// Fencing"): the key, the token its grant returned and the holder the caller is (`<plugin>/host`, or
/// `<plugin>.<service>/<replica>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseFence {
    pub key: String,
    pub token: i64,
    pub holder: String,
}

/// The most messages one transaction may carry, and the most bytes: the kernel holds them until the commit.
pub const MAX_TX_PUBLISHES: usize = 1_000;
pub const MAX_TX_PUBLISH_BYTES: usize = 16 * 1024 * 1024;

/// A message queued in a transaction: written to its topic by the kernel's own transaction when, and only when,
/// the plugin's transaction commits (docs/data.md, "Durable topics / Publishing with a write"). `by` is the
/// `published_by` a `topics.publish` records: the caller's principal.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingPublish {
    pub topic: String,
    pub payload: Value,
    pub by: String,
}

/// The error of a lease a transaction does not hold: the same shape a fenced batch gives (`CONFLICT`, the
/// failing step at index 0, and `condition: "lease"`).
fn lease_lost() -> Error {
    Error::ConflictWith {
        message: "transaction lease condition failed".into(),
        details: serde_json::json!({ "index": 0, "condition": "lease" }),
    }
}

/// Whether `lease` is the live grant of its holder, as the plugin's transaction must find it at commit. The
/// row is locked until the kernel's transaction ends, so it cannot be released or taken over under it.
async fn lock_lease(conn: &mut PgConnection, plugin: &str, lease: &LeaseFence) -> Result<()> {
    let live: Option<bool> = sqlx::query_scalar(
        "SELECT expires_at > clock_timestamp() FROM rookery.leases \
         WHERE name = $1 AND fence = $2 AND (holder = $3 OR starts_with(holder, $3 || '/')) FOR UPDATE",
    )
    .bind(format!("{plugin}/{}", lease.key))
    .bind(lease.token)
    .bind(&lease.holder)
    .fetch_optional(&mut *conn)
    .await?;
    match live {
        Some(true) => Ok(()),
        _ => Err(lease_lost()),
    }
}

/// An open transaction: one connection for its duration.
pub struct Tx {
    pub(crate) plugin: String,
    conn: Mutex<Option<PoolConnection<Postgres>>>,
    started: Instant,
    total: Duration,
    idle: Duration,
    last: std::sync::Mutex<Instant>,
    ended: std::sync::atomic::AtomicBool,
    /// The lease the transaction commits only while it holds, if it was opened with one.
    lease: Option<LeaseFence>,
    /// The messages the transaction publishes when it commits (in order), and their total size.
    pending: std::sync::Mutex<(Vec<PendingPublish>, usize)>,
}

impl Tx {
    /// Ends the transaction without committing: the connection is closed, which rolls it back.
    pub(crate) async fn abort(&self) {
        self.ended.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(mut c) = self.conn.lock().await.take() {
            c.close_on_drop();
        }
    }
    fn touch(&self) {
        *self.last.lock().expect("tx clock") = Instant::now();
    }
    fn deadline(&self) -> Instant {
        let last = *self.last.lock().expect("tx clock");
        (self.started + self.total).min(last + self.idle)
    }
}

impl Sql<'_> {
    /// Opens a transaction on its own connection and returns its id. At most [`MAX_TX`] per plugin; one
    /// lives [`TX_TOTAL`] and may wait [`TX_IDLE`] between statements. With a `lease`, the transaction commits
    /// only while the caller still holds it (docs/platform-split.md, "Leases / Fencing"); a lease that is not
    /// held now fails here already, and it is checked again at commit.
    pub async fn begin(&self, plugin: &str, lease: Option<LeaseFence>) -> Result<String> {
        {
            let txs = self.store.data.sql.txs.lock().expect("txs lock");
            if txs.values().filter(|t| t.plugin == plugin).count() >= MAX_TX {
                return Err(Error::Unavailable(format!(
                    "a plugin has at most {MAX_TX} open transactions"
                )));
            }
        }
        if let Some(lease) = &lease {
            // Fail fast, without a lock, on a lease the caller does not hold: the row is checked again, locked,
            // at commit. This is the kernel's own connection: the plugin's role cannot read `rookery.leases`.
            let live: Option<bool> = sqlx::query_scalar(
                "SELECT expires_at > clock_timestamp() FROM rookery.leases \
                 WHERE name = $1 AND fence = $2 AND (holder = $3 OR starts_with(holder, $3 || '/'))",
            )
            .bind(format!("{plugin}/{}", lease.key))
            .bind(lease.token)
            .bind(&lease.holder)
            .fetch_optional(self.store.pool())
            .await?;
            if live != Some(true) {
                return Err(lease_lost());
            }
        }
        let mut conn = self.connection(plugin, false).await?;
        conn.execute("BEGIN").await.map_err(map_error)?;
        let id = random_token("tx_");
        let tx = Arc::new(Tx {
            plugin: plugin.to_string(),
            conn: Mutex::new(Some(conn)),
            started: Instant::now(),
            total: self.limits().tx_total,
            idle: self.limits().tx_idle,
            last: std::sync::Mutex::new(Instant::now()),
            ended: std::sync::atomic::AtomicBool::new(false),
            lease,
            pending: std::sync::Mutex::new((Vec::new(), 0)),
        });
        let full = {
            let mut txs = self.store.data.sql.txs.lock().expect("txs lock");
            let full = txs.values().filter(|t| t.plugin == plugin).count() >= MAX_TX;
            if !full {
                txs.insert(id.clone(), tx.clone());
            }
            full
        };
        if full {
            tx.abort().await;
            return Err(Error::Unavailable(format!(
                "a plugin has at most {MAX_TX} open transactions"
            )));
        }
        // The clock: a transaction past its time (or idle too long) is rolled back.
        let store = self.store.clone();
        let key = id.clone();
        tokio::spawn(async move {
            loop {
                let deadline = tx.deadline();
                tokio::time::sleep_until(deadline.into()).await;
                if tx.ended.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                if tx.deadline() <= Instant::now() {
                    tx.abort().await;
                    store.data.sql.txs.lock().expect("txs lock").remove(&key);
                    return;
                }
            }
        });
        Ok(id)
    }

    fn tx(&self, plugin: &str, id: &str) -> Result<Arc<Tx>> {
        let txs = self.store.data.sql.txs.lock().expect("txs lock");
        match txs.get(id) {
            Some(t) if t.plugin == plugin => Ok(t.clone()),
            _ => Err(Error::not_found(
                "no such open transaction (it may have ended: a transaction lives 30 s and may be idle for 10 s)",
            )),
        }
    }

    fn forget_tx(&self, id: &str) {
        self.store.data.sql.txs.lock().expect("txs lock").remove(id);
    }

    /// A statement that returns rows, in the transaction.
    pub async fn tx_query(
        &self,
        plugin: &str,
        id: &str,
        sql: &str,
        params: &[Value],
    ) -> Result<Rows> {
        let tx = self.tx(plugin, id)?;
        let mut guard = tx.conn.lock().await;
        let Some(conn) = guard.as_mut() else {
            self.forget_tx(id);
            return Err(Error::not_found("the transaction has ended"));
        };
        tx.touch();
        let command = self.write_command(conn, sql, params, true).await;
        let out =
            tokio::time::timeout(self.limits().client(), collect(conn, sql, params, command)).await;
        let r = match out {
            Ok(r) => r,
            Err(_) => {
                if let Some(mut c) = guard.take() {
                    c.close_on_drop();
                }
                self.forget_tx(id);
                return Err(timed_out());
            }
        };
        tx.touch();
        r
    }

    /// A statement that returns a count, in the transaction.
    pub async fn tx_execute(
        &self,
        plugin: &str,
        id: &str,
        sql: &str,
        params: &[Value],
    ) -> Result<u64> {
        let tx = self.tx(plugin, id)?;
        let mut guard = tx.conn.lock().await;
        let Some(conn) = guard.as_mut() else {
            self.forget_tx(id);
            return Err(Error::not_found("the transaction has ended"));
        };
        tx.touch();
        let out = tokio::time::timeout(self.limits().client(), affected(conn, sql, params)).await;
        let r = match out {
            Ok(r) => r,
            Err(_) => {
                if let Some(mut c) = guard.take() {
                    c.close_on_drop();
                }
                self.forget_tx(id);
                return Err(timed_out());
            }
        };
        tx.touch();
        r
    }

    /// Queues a topic message for the transaction: the kernel writes it (and numbers it, exactly as
    /// `topics.publish` would) when the transaction commits, and drops it on rollback. The topic and its schema
    /// are checked by the caller, at the time of this call. In memory, bounded by [`MAX_TX_PUBLISHES`] and
    /// [`MAX_TX_PUBLISH_BYTES`].
    pub async fn tx_publish(
        &self,
        plugin: &str,
        id: &str,
        topic: &str,
        payload: &Value,
        by: &str,
    ) -> Result<()> {
        let tx = self.tx(plugin, id)?;
        tx.touch();
        let size = serde_json::to_vec(payload)
            .map_err(|e| Error::invalid(e.to_string()))?
            .len();
        let mut pending = tx.pending.lock().expect("pending publishes");
        if pending.0.len() >= MAX_TX_PUBLISHES {
            return Err(Error::invalid(format!(
                "a transaction publishes at most {MAX_TX_PUBLISHES} messages"
            )));
        }
        if pending.1 + size > MAX_TX_PUBLISH_BYTES {
            return Err(Error::invalid(format!(
                "a transaction's messages total at most {} MiB",
                MAX_TX_PUBLISH_BYTES / (1024 * 1024)
            )));
        }
        pending.0.push(PendingPublish {
            topic: topic.to_string(),
            payload: payload.clone(),
            by: by.to_string(),
        });
        pending.1 += size;
        Ok(())
    }

    /// Whether the transaction has messages to publish if it commits.
    pub fn tx_has_publishes(&self, plugin: &str, id: &str) -> bool {
        self.tx(plugin, id)
            .is_ok_and(|t| !t.pending.lock().expect("pending publishes").0.is_empty())
    }

    /// Commits the transaction.
    pub async fn commit(&self, plugin: &str, id: &str) -> Result<()> {
        self.end(plugin, id, true).await
    }

    /// Rolls the transaction back.
    pub async fn rollback(&self, plugin: &str, id: &str) -> Result<()> {
        self.end(plugin, id, false).await
    }

    async fn end(&self, plugin: &str, id: &str, commit: bool) -> Result<()> {
        let tx = self.tx(plugin, id)?;
        self.forget_tx(id);
        tx.ended.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut guard = tx.conn.lock().await;
        let Some(mut conn) = guard.take() else {
            return Err(Error::not_found("the transaction has ended"));
        };
        let publishes = !tx.pending.lock().expect("pending publishes").0.is_empty();
        if commit && (tx.lease.is_some() || publishes) {
            return self.two_step_commit(plugin, &tx, &mut conn).await;
        }
        let r = conn
            .execute(if commit { "COMMIT" } else { "ROLLBACK" })
            .await;
        match r {
            Ok(_) => Ok(()),
            Err(e) => {
                conn.close_on_drop();
                Err(map_error(e))
            }
        }
    }

    /// Commits a transaction that has a lease to check or messages to publish: the kernel's own transaction
    /// locks the lease row and writes the messages, the plugin's transaction commits under it, and the kernel's
    /// commits last (see the module docs).
    async fn two_step_commit(
        &self,
        plugin: &str,
        tx: &Tx,
        conn: &mut PoolConnection<Postgres>,
    ) -> Result<()> {
        let mut kernel = match self.store.pool().acquire().await {
            Ok(k) => k,
            Err(e) => {
                conn.close_on_drop();
                return Err(e.into());
            }
        };
        let pending = tx.pending.lock().expect("pending publishes").0.clone();
        // A lease row a release or a takeover is writing must not park the kernel's connection for the length of
        // that statement (the plugin's transaction is still open while we wait).
        let prepared: Result<()> = async {
            kernel.execute("BEGIN").await.map_err(map_error)?;
            sqlx::query("SET LOCAL lock_timeout = '5s'")
                .execute(&mut *kernel)
                .await
                .map_err(map_error)?;
            if let Some(lease) = &tx.lease {
                lock_lease(&mut kernel, plugin, lease).await?;
            }
            // The messages go in last, as a batch writes its publish step last: the topic's advisory lock is
            // held until the kernel's transaction ends, and the plugin's commit is one round trip away.
            for p in &pending {
                crate::topics::publish_on(&mut kernel, &p.topic, &p.payload, &p.by).await?;
            }
            Ok(())
        }
        .await;
        if let Err(e) = prepared {
            let _ = kernel.execute("ROLLBACK").await;
            conn.close_on_drop();
            return Err(e);
        }
        // The plugin's transaction, while the lease row is locked: no release and no takeover can pass the lock
        // until the kernel's transaction below ends.
        if let Err(e) = conn.execute("COMMIT").await {
            let _ = kernel.execute("ROLLBACK").await;
            conn.close_on_drop();
            return Err(map_error(e));
        }
        match kernel.execute("COMMIT").await {
            Ok(_) => Ok(()),
            Err(e) => {
                // The plugin's data is committed; the lease check and the messages are not. Nothing can take
                // them back, so say what happened instead of pretending the transaction failed.
                kernel.close_on_drop();
                Err(Error::Unavailable(format!(
                    "the transaction committed, but the kernel could not commit its lease check and messages: {}",
                    short(&e.to_string())
                )))
            }
        }
    }

    /// Cancels what runs past its limit, for every plugin's role, including sessions that changed their own
    /// timeouts and direct connections: a statement active longer than its limit is cancelled, a
    /// transaction idle longer than 15 s ends. On a plugin without `database.direct` it also ends any session
    /// the kernel did not open. The kernel runs this every few seconds. Returns how many sessions it ended or
    /// cancelled.
    pub async fn enforce_limits(&self) -> Result<u64> {
        Ok(self.enforce().await?.ended)
    }

    /// [`Sql::enforce_limits`], with what it found: for the kernel to log.
    pub async fn enforce(&self) -> Result<Enforced> {
        let pool = self.store.pool();
        // A deployment that has no plugin with raw SQL has nothing to watch (and nothing to clean up).
        let any: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM rookery.plugin_sql) OR EXISTS (SELECT 1 FROM rookery.plugin_sql_sessions)",
        )
        .fetch_one(pool)
        .await?;
        if !any {
            return Ok(Enforced::default());
        }
        // Registrations of sessions that are gone (a kernel that stopped, a closed pool).
        sqlx::query(
            "DELETE FROM rookery.plugin_sql_sessions r WHERE NOT EXISTS \
             (SELECT 1 FROM pg_stat_activity a WHERE a.pid = r.pid AND a.backend_start = r.backend_start)",
        )
        .execute(pool)
        .await?;
        let rows = sqlx::query(
            "SELECT a.pid, a.backend_start, a.usename::text AS role, s.plugin, s.direct, a.state, r.purpose, \
                    EXTRACT(EPOCH FROM now() - a.query_start)::float8 AS active_secs, \
                    EXTRACT(EPOCH FROM now() - a.state_change)::float8 AS idle_secs, \
                    EXTRACT(EPOCH FROM now() - a.backend_start)::float8 AS age_secs \
             FROM pg_stat_activity a JOIN rookery.plugin_sql s ON s.role_name = a.usename \
             LEFT JOIN rookery.plugin_sql_sessions r ON r.pid = a.pid AND r.backend_start = a.backend_start \
             WHERE a.pid <> pg_backend_pid() AND a.datname = current_database()",
        )
        .fetch_all(pool)
        .await?;
        let limits = self.limits();
        let statement = limits.statement.as_secs_f64() + 2.0;
        let migration = super::migrations::MIGRATION_TIMEOUT_SECS as f64 + 2.0;
        let idle_limit = limits.idle_in_transaction.as_secs_f64() + 1.0;
        let grace = REGISTRATION_GRACE.as_secs_f64();
        let mut out = Enforced::default();
        let mut foreign: std::collections::BTreeMap<String, (String, u64)> = Default::default();
        for r in rows {
            let pid: i32 = r.try_get("pid")?;
            let started: chrono::DateTime<chrono::Utc> = r.try_get("backend_start")?;
            let plugin: String = r.try_get("plugin")?;
            let role: String = r.try_get("role")?;
            let direct: bool = r.try_get("direct")?;
            let state: Option<String> = r.try_get("state")?;
            let purpose: Option<String> = r.try_get("purpose")?;
            let active = r.try_get::<Option<f64>, _>("active_secs")?.unwrap_or(0.0);
            let idle = r.try_get::<Option<f64>, _>("idle_secs")?.unwrap_or(0.0);
            let age = r.try_get::<Option<f64>, _>("age_secs")?.unwrap_or(0.0);
            let state = state.unwrap_or_default();
            // `terminate`/`cancel` re-check the start time: the pid is the same session that was looked at.
            let end = |terminate: bool| {
                sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(if terminate {
                    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE pid = $1 AND backend_start = $2"
                } else {
                    "SELECT pg_cancel_backend(pid) FROM pg_stat_activity WHERE pid = $1 AND backend_start = $2"
                }.to_string()))
                .bind(pid)
                .bind(started)
                .fetch_optional(pool)
            };
            if purpose.is_none() && !direct && age > grace {
                // A session of this role that the kernel did not open, where the manifest gave the plugin no direct
                // connections: the plugin opened it itself (a role can change its own password).
                end(true).await?;
                out.ended += 1;
                foreign.entry(plugin).or_insert((role, 0)).1 += 1;
                continue;
            }
            let limit = if purpose.as_deref() == Some("migration") {
                migration
            } else {
                statement
            };
            if state == "active" && active > limit {
                end(false).await?;
                out.ended += 1;
            } else if state.starts_with("idle in transaction") && idle > idle_limit {
                end(true).await?;
                out.ended += 1;
            }
        }
        for (plugin, (role, sessions)) in foreign {
            // The password goes back to the kernel's, so the next login with the one the plugin chose fails.
            let restored = self.restore_password(&plugin).await.is_ok();
            let first: Option<bool> = sqlx::query_scalar(
                "UPDATE rookery.plugin_sql SET direct_bypass_reported_at = now() \
                 WHERE plugin = $1 AND direct_bypass_reported_at IS NULL RETURNING true",
            )
            .bind(&plugin)
            .fetch_optional(pool)
            .await?;
            if first.is_some() {
                let mut conn = pool.acquire().await?;
                crate::audit::record_on(
                    &mut conn,
                    "system:rookery",
                    "sql.directSessionEnded",
                    Some(&format!("plugin://{plugin}")),
                    serde_json::json!({
                        "role": role,
                        "sessions": sessions,
                        "passwordRestored": restored,
                        "reason": "a session of the plugin's role that the kernel did not open, on a plugin without database.direct",
                    }),
                )
                .await?;
            }
            out.foreign.push(Foreign {
                plugin,
                role,
                sessions,
                first: first.is_some(),
            });
        }
        Ok(out)
    }
}

/// What one pass of the watchdog did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Enforced {
    /// Sessions cancelled or ended, for any reason.
    pub ended: u64,
    /// Plugins without `database.direct` whose role had sessions the kernel did not open.
    pub foreign: Vec<Foreign>,
}

/// A plugin that opened sessions as its own role without being given a direct connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Foreign {
    pub plugin: String,
    pub role: String,
    /// Sessions ended in this pass.
    pub sessions: u64,
    /// Whether this pass wrote the audit entry (the first since the credential changed).
    pub first: bool,
}

/// Writes down the session `conn` just opened as `role`: its backend PID (asked of the connection before any
/// statement of the plugin ran on it) and the start time of that backend, which the kernel reads from
/// `pg_stat_activity` as itself. `purpose` is `pool` or `migration`. Fails the connection when it cannot.
pub(crate) async fn register_session(
    kernel: &PgPool,
    conn: &mut PgConnection,
    role: &str,
    purpose: &str,
) -> std::result::Result<(), sqlx::Error> {
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *conn)
        .await?;
    let n = sqlx::query(
        "INSERT INTO rookery.plugin_sql_sessions (pid, backend_start, role_name, purpose) \
         SELECT pid, backend_start, usename::text, $3 FROM pg_stat_activity \
         WHERE pid = $1 AND usename = $2 AND datname = current_database() AND backend_start IS NOT NULL \
         ON CONFLICT (pid, backend_start) DO UPDATE SET purpose = EXCLUDED.purpose",
    )
    .bind(pid)
    .bind(role)
    .bind(purpose)
    .execute(kernel)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(sqlx::Error::Protocol(
            "the kernel could not see the session it opened in pg_stat_activity".into(),
        ));
    }
    Ok(())
}
