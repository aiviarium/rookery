//! `sql.*`: raw SQL of a plugin whose manifest says `database.sql`. What its role may and may not do is
//! the store's (`rookery_store::sql`); this is the server's side:
//!
//! - when a plugin is applied ([`crate::plugins::apply`]) its role and grants are made right, its views
//!   follow its collections, and its pending SQL migrations run; a plugin that lost `database.sql` loses
//!   its role's access (the data stays);
//! - a watchdog ends statements and idle transactions past their limits, direct connections included;
//! - a plugin calls the `sql.*` methods for itself; an admin reads and rotates a direct credential for a
//!   service the server does not start.

use std::collections::BTreeSet;
use std::time::Duration;

use futures_util::StreamExt;
use rookery_kernel::streams::Initiator;
use rookery_kernel::{Call, Caller, Empty, Kernel, Level, Methods, RpcError};
use rookery_proto::sql::{
    SqlBegin, SqlColumn, SqlCredential, SqlCredentialRef, SqlExecuted, SqlPublished, SqlQuery,
    SqlRows, SqlStatement, SqlStreamed, SqlTx, SqlTxPublish, SqlTxRef, SqlTxStatement,
};
use rookery_store::sql::exec::{Column, LeaseFence, Rows, Written};
use rookery_store::sql::migrations::SqlMigration;
use serde_json::json;

use crate::rpc;

/// How often the watchdog looks for statements and idle transactions past their limits.
const WATCHDOG_EVERY: Duration = Duration::from_secs(3);

/// What a plugin's manifest says of raw SQL (`database`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Database {
    /// `database.sql`: a role, a raw schema and views over its collections.
    pub sql: bool,
    /// `database.direct`: its services connect as the role themselves.
    pub direct: bool,
    /// `database.migrations` is `service`: the service migrates the raw schema, not `migrations/sql/`.
    pub service_migrations: bool,
}

/// The SQL migrations in `<dir>/migrations/sql/`, in order.
pub fn read_sql_migrations(dir: &std::path::Path) -> Result<Vec<SqlMigration>, RpcError> {
    rookery_store::sql::migrations::read_dir(dir).map_err(rpc)
}

/// The topics each open transaction published to, whose streams wake when it commits.
#[derive(Default)]
struct Published(std::sync::Mutex<std::collections::HashMap<String, BTreeSet<String>>>);

/// The plugins this server gave raw SQL to while it runs, to take it away from one that drops it.
#[derive(Default)]
struct Enabled(std::sync::Mutex<BTreeSet<String>>);

/// Makes a plugin's raw SQL what its manifest says: called by [`crate::plugins::apply`].
pub(crate) async fn sync(
    kernel: &Kernel,
    plugin: &str,
    database: &Database,
    files: &[SqlMigration],
) -> Result<(), RpcError> {
    let sql = kernel.store.sql();
    if !database.sql {
        if !files.is_empty() {
            return Err(RpcError::invalid(format!(
                "migrations/sql has {} file(s), but the manifest has no database.sql: add \"database\": {{ \"sql\": true }}",
                files.len()
            )));
        }
        let had = kernel
            .facility::<Enabled>()
            .0
            .lock()
            .unwrap()
            .remove(plugin);
        if had || sql.is_enabled(plugin).await.map_err(rpc)? {
            sql.disable(plugin).await.map_err(rpc)?;
        }
        return Ok(());
    }
    if database.service_migrations && !files.is_empty() {
        return Err(RpcError::invalid(
            "database.migrations is \"service\": the service migrates the raw schema, and migrations/sql is empty",
        ));
    }
    sql.enable(plugin, database.direct).await.map_err(rpc)?;
    kernel
        .facility::<Enabled>()
        .0
        .lock()
        .unwrap()
        .insert(plugin.to_owned());
    if files.is_empty() {
        return Ok(());
    }
    let ran = sql.migrate(plugin, files).await.map_err(rpc)?;
    if !ran.is_empty() {
        tracing::info!(plugin, versions = ?ran, "SQL migrations ran");
    }
    Ok(())
}

/// Takes raw SQL away from the plugins not in `keep`, such as those removed while the server was down:
/// the plugin system calls it when it starts, with the plugins whose manifests say `database.sql`.
/// With some to keep, it first makes sure no role may take the server's advisory locks; when it cannot,
/// raw SQL stays off for all of them.
pub async fn sweep(kernel: &Kernel, keep: BTreeSet<String>) -> Result<Vec<String>, RpcError> {
    let sql = kernel.store.sql();
    let keep = if keep.is_empty() {
        keep
    } else {
        match sql.seal_advisory_locks().await {
            Ok(()) => keep,
            Err(e) => {
                tracing::error!(error = %e, "raw SQL stays off: the roles of plugins are ended");
                BTreeSet::new()
            }
        }
    };
    if !keep.is_empty() {
        let pool = i64::from(kernel.store.pool().options().get_max_connections()) + 1;
        if let Ok(h) = sql.connection_headroom().await {
            for w in h.warnings(pool) {
                tracing::warn!("{w}");
            }
        }
    }
    sql.sweep(&keep).await.map_err(rpc)
}

pub fn register(methods: &mut Methods) {
    methods.on_started(|kernel| {
        let k = kernel.clone();
        tokio::spawn(async move { watchdog(&k).await });
    });
    methods
        .add("sql.query", query)
        .summary(
            "Rows of a statement in the plugin's own schema or over its views, at most 10,000.",
        )
        .level(Level::Plugin);
    methods
        .add("sql.execute", execute)
        .summary("Runs a statement in the plugin's own schema: the number of rows it touched.")
        .level(Level::Plugin);
    methods
        .add("sql.queryStream", query_stream)
        .summary("The rows of a statement as lines of JSON on a stream.")
        .level(Level::Plugin)
        .stream();
    methods
        .add("sql.begin", begin)
        .summary("A transaction of the plugin's, at most four open; with a lease, it commits only while the caller holds it.")
        .level(Level::Plugin);
    methods
        .add("sql.txQuery", tx_query)
        .summary("Rows of a statement in a transaction.")
        .level(Level::Plugin);
    methods
        .add("sql.txExecute", tx_execute)
        .summary("Runs a statement in a transaction.")
        .level(Level::Plugin);
    methods
        .add("sql.txPublish", tx_publish)
        .summary(
            "A message of the plugin's topic, published if and only if the transaction commits.",
        )
        .level(Level::Plugin);
    methods
        .add("sql.commit", commit)
        .summary("Commits a transaction.")
        .level(Level::Plugin);
    methods
        .add("sql.rollback", rollback)
        .summary("Rolls a transaction back.")
        .level(Level::Plugin);
    methods
        .add("sql.credential", |call: Call, p: SqlCredentialRef| async move {
            credential(&call, &p.plugin, false).await
        })
        .summary("The connection string of a plugin with direct connections, for a service the server does not start.")
        .level(Level::Admin);
    methods
        .add("sql.rotateCredential", |call: Call, p: SqlCredentialRef| async move {
            credential(&call, &p.plugin, true).await
        })
        .summary("Gives a plugin's role a new password, which ends its sessions, and returns the new connection string.")
        .level(Level::Admin);
}

/// The plugin a `sql.*` call is for: its own.
fn own(call: &Call) -> Result<String, RpcError> {
    match &call.caller {
        Caller::Plugin(p) => Ok(p.clone()),
        _ => Err(RpcError::forbidden("raw SQL is a plugin's own")),
    }
}

fn columns_of(c: Vec<Column>) -> Vec<SqlColumn> {
    c.into_iter()
        .map(|c| SqlColumn {
            name: c.name,
            ty: c.ty,
        })
        .collect()
}

fn rows_of(r: Rows) -> SqlRows {
    SqlRows {
        columns: columns_of(r.columns),
        rows: r.rows,
    }
}

/// Who ran what: writes, commits and the writes a query turned out to make. The statement is cut and
/// its parameters left out.
async fn audit(call: &Call, action: &str, sql: &str, written: Option<&Written>) {
    let text: String = sql.chars().take(200).collect();
    let mut details = json!({ "statement": text });
    if let Some(w) = written {
        details["command"] = json!(w.command);
        details["rows"] = json!(w.rows);
    }
    let resource = format!("plugin://{}", own(call).unwrap_or_default());
    call.audit(action, Some(&resource), details).await;
}

async fn query(call: Call, p: SqlQuery) -> Result<SqlRows, RpcError> {
    let plugin = own(&call)?;
    let rows = call
        .store()
        .sql()
        .query(&plugin, &p.sql, &p.params, p.replica)
        .await
        .map_err(rpc)?;
    // A statement that returns rows can still write (`insert ... returning`); its plan says so.
    if let Some(written) = &rows.written {
        audit(&call, "sql.query", &p.sql, Some(written)).await;
    }
    Ok(rows_of(rows))
}

async fn execute(call: Call, p: SqlStatement) -> Result<SqlExecuted, RpcError> {
    let plugin = own(&call)?;
    let n = call
        .store()
        .sql()
        .execute(&plugin, &p.sql, &p.params)
        .await
        .map_err(rpc)?;
    audit(&call, "sql.execute", &p.sql, None).await;
    Ok(SqlExecuted { rows: n })
}

async fn query_stream(call: Call, p: SqlQuery) -> Result<SqlStreamed, RpcError> {
    let plugin = own(&call)?;
    let stream = call.conn()?.streams().open(Initiator::Peer)?;
    let (columns, mut rows, command) = match call
        .store()
        .sql()
        .query_stream(&plugin, &p.sql, &p.params, p.replica)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            stream.close(None);
            return Err(rpc(e));
        }
    };
    let id = stream.id();
    tokio::spawn(async move {
        let mut sent = 0u64;
        let mut failed = None;
        while let Some(row) = rows.next().await {
            match row {
                Ok(row) => {
                    let mut line = serde_json::to_vec(&row).unwrap_or_default();
                    line.push(b'\n');
                    if stream.write(&line).await.is_err() {
                        break;
                    }
                    sent += 1;
                }
                Err(e) => {
                    failed = Some(RpcError::from(e).body());
                    break;
                }
            }
        }
        if failed.is_none() {
            stream.eof();
        }
        stream.close(failed);
        // The statement ran, so its write is audited, with the rows it gave.
        if let Some(command) = command {
            let written = Written {
                command,
                rows: sent,
            };
            audit(&call, "sql.queryStream", &p.sql, Some(&written)).await;
        }
    });
    Ok(SqlStreamed {
        stream: id,
        columns: columns_of(columns),
    })
}

async fn begin(call: Call, p: SqlBegin) -> Result<SqlTx, RpcError> {
    let plugin = own(&call)?;
    // The holder is the caller, or one of its workers: `<caller>/<worker>`.
    let fence = p.lease.map(|l| LeaseFence {
        key: l.key,
        token: l.token,
        holder: call.audit_name(),
    });
    let tx = call
        .store()
        .sql()
        .begin(&plugin, fence)
        .await
        .map_err(rpc)?;
    Ok(SqlTx { tx })
}

async fn tx_query(call: Call, p: SqlTxStatement) -> Result<SqlRows, RpcError> {
    let plugin = own(&call)?;
    let rows = call
        .store()
        .sql()
        .tx_query(&plugin, &p.tx, &p.sql, &p.params)
        .await
        .map_err(rpc)?;
    if let Some(written) = &rows.written {
        audit(&call, "sql.txQuery", &p.sql, Some(written)).await;
    }
    Ok(rows_of(rows))
}

async fn tx_execute(call: Call, p: SqlTxStatement) -> Result<SqlExecuted, RpcError> {
    let plugin = own(&call)?;
    let n = call
        .store()
        .sql()
        .tx_execute(&plugin, &p.tx, &p.sql, &p.params)
        .await
        .map_err(rpc)?;
    audit(&call, "sql.txExecute", &p.sql, None).await;
    Ok(SqlExecuted { rows: n })
}

async fn tx_publish(call: Call, p: SqlTxPublish) -> Result<SqlPublished, RpcError> {
    let plugin = own(&call)?;
    // A plugin publishes to its own topics, `<plugin>/<name>`.
    let name = p
        .topic
        .strip_prefix(&format!("{plugin}/"))
        .unwrap_or(&p.topic);
    if name.is_empty() || name.len() > 200 || name.contains('/') {
        return Err(RpcError::invalid(format!(
            "{:?} is no topic of the plugin's",
            p.topic
        )));
    }
    let topic = format!("{plugin}/{name}");
    if serde_json::to_vec(&p.payload).map(|v| v.len()).unwrap_or(0)
        > rookery_store::collections::MAX_TOPIC_PAYLOAD
    {
        return Err(RpcError::invalid("a message of a topic is at most 256 KiB"));
    }
    call.store()
        .sql()
        .tx_publish(&plugin, &p.tx, &topic, &p.payload, &call.audit_name())
        .await
        .map_err(rpc)?;
    call.kernel
        .facility::<Published>()
        .0
        .lock()
        .unwrap()
        .entry(p.tx.clone())
        .or_default()
        .insert(topic);
    Ok(SqlPublished {})
}

async fn commit(call: Call, p: SqlTxRef) -> Result<Empty, RpcError> {
    let plugin = own(&call)?;
    let topics = call
        .kernel
        .facility::<Published>()
        .0
        .lock()
        .unwrap()
        .remove(&p.tx)
        .unwrap_or_default();
    call.store()
        .sql()
        .commit(&plugin, &p.tx)
        .await
        .map_err(rpc)?;
    audit(&call, "sql.commit", "", None).await;
    // The messages are there now: the streams of their topics look again.
    for topic in &topics {
        rookery_jobs::topics::wake_topic(&call.kernel, topic);
    }
    Ok(Empty {})
}

async fn rollback(call: Call, p: SqlTxRef) -> Result<Empty, RpcError> {
    let plugin = own(&call)?;
    call.kernel
        .facility::<Published>()
        .0
        .lock()
        .unwrap()
        .remove(&p.tx);
    call.store()
        .sql()
        .rollback(&plugin, &p.tx)
        .await
        .map_err(rpc)?;
    Ok(Empty {})
}

async fn credential(call: &Call, plugin: &str, rotate: bool) -> Result<SqlCredential, RpcError> {
    let store = call.store();
    if rotate {
        store.sql().rotate(plugin).await.map_err(rpc)?;
    }
    let url = store
        .sql()
        .connection_url(plugin)
        .await
        .map_err(rpc)?
        .ok_or_else(|| {
            RpcError::not_found(format!(
                "the plugin {plugin} has no direct connections (database.direct)"
            ))
        })?;
    let role = store.sql().names(plugin).map_err(rpc)?.role;
    let action = if rotate {
        "sql.rotateCredential"
    } else {
        "sql.credential"
    };
    call.audit(
        action,
        Some(&format!("plugin://{plugin}")),
        json!({ "role": role }),
    )
    .await;
    Ok(SqlCredential { url, role })
}

/// Ends statements and idle transactions that run past their limits, on every connection of a role.
async fn watchdog(kernel: &Kernel) {
    let mut tick = tokio::time::interval(WATCHDOG_EVERY);
    loop {
        tokio::select! {
            _ = kernel.shutdown.cancelled() => return,
            _ = tick.tick() => {}
        }
        match kernel.store.sql().enforce().await {
            Ok(done) => {
                for f in &done.foreign {
                    tracing::warn!(
                        plugin = %f.plugin, role = %f.role, sessions = f.sessions, first = f.first,
                        "sessions the server did not open were ended on a role without database.direct; its password was set again"
                    );
                }
                if done.ended > 0 {
                    tracing::warn!(
                        sessions = done.ended,
                        "raw SQL sessions past their limits were ended"
                    );
                }
            }
            Err(e) => tracing::debug!(error = %e, "the raw SQL watchdog failed"),
        }
    }
}
