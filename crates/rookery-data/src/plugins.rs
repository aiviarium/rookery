//! The runtime of what plugins declare of their data: their collections (`contributes.collections`)
//! and their numbered migrations (`migrations/<version>.json`).
//!
//! The plugin system calls [`apply`] when a plugin loads, with what it read from the plugin's package
//! and a [`Transforms`] that runs TypeScript steps in the extension host. `apply` runs the migrations
//! newer than the plugin's schema version, each in a transaction of its own, then reconciles the
//! manifest's collections: additive changes are made as they are, anything else fails with the list of
//! differences (`INVALID`, `details.differences`), so the plugin does not activate.
//!
//! On a first install the migrations are skipped: the plugin starts at the newest, since its manifest
//! already describes their result. A migration marked `onInstall` is not skipped: it adopts data from
//! outside the plugin's collections, which a first install is when that data is there.
//!
//! A `transform` step `{ "op": "transform", "name": "fill" }` runs `migrations/fill.ts`. The server makes
//! the migration an id and asks [`Transforms::run`] to run the step with it; the step's `MigrationTx`
//! calls `data.txRows`, `data.txLegacy`, `data.txInsert`, `data.txUpdate` and `data.txDelete` with that
//! id, as the plugin, and each goes into the migration's transaction until `run` returns.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use rookery_kernel::{Call, Caller, Kernel, Level, Methods, RpcError};
use rookery_proto::data::{TxDelete, TxInsert, TxLegacy, TxRows, TxUpdate};
use rookery_store::collections::{CollectionDef, MigrationTx, PluginMigration, Transformer};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::{parse_id, rpc};

/// How long one transform step may run.
const TRANSFORM_TIMEOUT: Duration = Duration::from_secs(600);

/// What a plugin declares of its data.
#[derive(Debug, Clone, Default)]
pub struct PluginData {
    /// The plugin's id: lower-case letters, digits and `-`. Its tables are in `p_<id>`.
    pub plugin: String,
    /// `contributes.collections` of its manifest, as it is.
    pub collections: Value,
    /// The collections only admins may read and write, such as those of an admin page.
    pub admin_only: Vec<String>,
    /// Its numbered migrations, from [`read_migrations`].
    pub migrations: Vec<PluginMigration>,
}

/// Runs a plugin's `transform` steps: the extension host's half of a migration.
#[async_trait::async_trait]
pub trait Transforms: Send + Sync {
    /// Runs `migrations/<name>.ts` of `plugin`, whose default export gets a `MigrationTx` that calls
    /// `data.tx*` with `migration`. Returns once the export has returned; an error fails the migration,
    /// and nothing of it is kept.
    async fn run(&self, plugin: &str, name: &str, migration: &str) -> Result<(), RpcError>;
}

/// For plugins without TypeScript: any transform step fails.
pub struct NoTransforms;

#[async_trait::async_trait]
impl Transforms for NoTransforms {
    async fn run(&self, plugin: &str, name: &str, _: &str) -> Result<(), RpcError> {
        Err(RpcError::unavailable(format!(
            "the migration step {name} of {plugin} needs the extension host"
        )))
    }
}

/// What [`apply`] did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Applied {
    /// The migrations that ran, by version.
    pub migrated: Vec<i64>,
    pub created: Vec<String>,
    /// `collection: change` lines.
    pub altered: Vec<String>,
}

/// Runs a plugin's migrations, then applies its collections.
pub async fn apply(
    kernel: &Kernel,
    data: &PluginData,
    transforms: &dyn Transforms,
) -> Result<Applied, RpcError> {
    let plugin = &data.plugin;
    if plugin == rookery_store::collections::system::PLUGIN {
        return Err(RpcError::invalid("the plugin rookery is Rookery's own"));
    }
    rookery_store::collections::validate_plugin_id(plugin).map_err(rpc)?;
    let mut defs: Vec<CollectionDef> = if data.collections.is_null() {
        Vec::new()
    } else {
        serde_json::from_value(data.collections.clone())
            .map_err(|e| RpcError::invalid(format!("contributes.collections: {e}")))?
    };
    for d in &mut defs {
        d.access.admin = data.admin_only.contains(&d.name);
    }
    let migrations = &data.migrations;
    check_versions(migrations)?;
    let store = &kernel.store;
    let collections = store.collections();
    let baseline_at = match migrations.iter().find(|m| m.on_install) {
        Some(first) => first.version - 1,
        None => migrations.last().map_or(0, |m| m.version),
    };
    let fresh = collections.is_fresh(plugin).await.map_err(rpc)?;
    let baselined = if baseline_at > 0 {
        collections
            .baseline(plugin, baseline_at)
            .await
            .map_err(rpc)?
    } else {
        false
    };
    // On a first install, the migrations that still run (`onInstall`) find the manifest's collections.
    let adopting = fresh && migrations.iter().any(|m| m.version > baseline_at);
    if adopting {
        collections
            .apply(plugin, &defs)
            .await
            .map_err(|e| context(e, "its collections cannot be applied"))?;
    }
    let mut applied = Applied::default();
    if !migrations.is_empty() && (!baselined || adopting) {
        let transformer = HostTransformer {
            kernel: kernel.clone(),
            plugin: plugin.clone(),
            transforms,
        };
        applied.migrated = collections
            .migrate(plugin, migrations, &transformer)
            .await
            .map_err(|e| context(e, "a migration failed"))?;
        if !applied.migrated.is_empty() {
            tracing::info!(plugin = %plugin, versions = ?applied.migrated, "migrations ran");
        }
    }
    if defs.is_empty() && collections.list(plugin).is_empty() {
        return Ok(applied);
    }
    let report = collections
        .apply(plugin, &defs)
        .await
        .map_err(|e| context(e, "its collections cannot be applied"))?;
    applied.created = report.created;
    applied.altered = report.altered;
    Ok(applied)
}

fn check_versions(migrations: &[PluginMigration]) -> Result<(), RpcError> {
    if let Some(w) = migrations.windows(2).find(|w| w[0].version >= w[1].version) {
        return Err(RpcError::invalid(format!(
            "migrations are in order of their versions, each once: {} before {}",
            w[0].version, w[1].version
        )));
    }
    Ok(())
}

fn context(e: rookery_store::Error, what: &str) -> RpcError {
    let mut err: RpcError = e.into();
    err.message = format!("{what}: {}", err.message);
    err
}

/// The numbered migrations in `<dir>/migrations/`, in order: `0002.json` is version 2 unless the file
/// says another.
pub fn read_migrations(dir: &Path) -> Result<Vec<PluginMigration>, RpcError> {
    let folder = dir.join("migrations");
    let Ok(entries) = std::fs::read_dir(&folder) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for e in entries.filter_map(|e| e.ok()) {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let digits: String = stem.chars().take_while(|c| c.is_ascii_digit()).collect();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let text = std::fs::read_to_string(&path)
            .map_err(|err| RpcError::invalid(format!("migrations/{name}: {err}")))?;
        let mut v: Value = serde_json::from_str(&text).map_err(|err| {
            RpcError::invalid(format!("migrations/{name} is not valid JSON: {err}"))
        })?;
        if v.get("version").is_none() {
            let version: i64 = digits.parse().map_err(|_| {
                RpcError::invalid(format!(
                    "migrations/{name}: name a migration by its number (0002.json) or give it a version"
                ))
            })?;
            v["version"] = json!(version);
        }
        let m: PluginMigration = serde_json::from_value(v)
            .map_err(|err| RpcError::invalid(format!("migrations/{name} is not valid: {err}")))?;
        out.push(m);
    }
    out.sort_by_key(|m| m.version);
    if let Some(w) = out.windows(2).find(|w| w[0].version == w[1].version) {
        return Err(RpcError::invalid(format!(
            "two migrations have the version {}",
            w[0].version
        )));
    }
    Ok(out)
}

// ── transforms in the extension host ─────────────────────────────────────────────────────────────

enum TxOp {
    Rows {
        collection: String,
        after: Option<uuid::Uuid>,
        limit: i64,
    },
    Legacy {
        table: String,
        after: Option<String>,
        limit: i64,
    },
    Insert {
        collection: String,
        values: Value,
        space: Option<uuid::Uuid>,
        user: Option<uuid::Uuid>,
    },
    Update {
        collection: String,
        id: uuid::Uuid,
        patch: Value,
    },
    Delete {
        collection: String,
        id: uuid::Uuid,
    },
}

type OpReply = oneshot::Sender<Result<Value, RpcError>>;
type OpSender = mpsc::Sender<(TxOp, OpReply)>;

/// The migrations whose transform runs: their id, to the plugin and the way into their transaction.
#[derive(Default)]
struct Running {
    migrations: Mutex<HashMap<String, (String, OpSender)>>,
}

pub fn register(methods: &mut Methods) {
    methods
        .add("data.txRows", |call: Call, p: TxRows| async move {
            let after = p.after.as_deref().map(|a| parse_id("after", a)).transpose()?;
            let op = TxOp::Rows {
                collection: p.collection,
                after,
                limit: p.limit.unwrap_or(500),
            };
            run_op(&call, &p.migration, op).await
        })
        .summary("A migration's transform: rows of a collection, in id order, in the migration's transaction.")
        .level(Level::Plugin);
    methods
        .add("data.txLegacy", |call: Call, p: TxLegacy| async move {
            let op = TxOp::Legacy {
                table: p.table,
                after: p.after,
                limit: p.limit.unwrap_or(500),
            };
            run_op(&call, &p.migration, op).await
        })
        .summary("A migration's transform: rows of a table a release of Rookery retired to rookery_legacy.")
        .level(Level::Plugin);
    methods
        .add("data.txInsert", |call: Call, p: TxInsert| async move {
            let op = TxOp::Insert {
                collection: p.collection,
                values: p.values,
                space: p.space.as_deref().map(|s| parse_id("space", s)).transpose()?,
                user: p.user.as_deref().map(|u| parse_id("user", u)).transpose()?,
            };
            run_op(&call, &p.migration, op).await
        })
        .summary("A migration's transform: inserts a row, in a space or of a person, in the migration's transaction.")
        .level(Level::Plugin);
    methods
        .add("data.txUpdate", |call: Call, p: TxUpdate| async move {
            let op = TxOp::Update {
                collection: p.collection,
                id: parse_id("id", &p.id)?,
                patch: p.patch,
            };
            run_op(&call, &p.migration, op).await
        })
        .summary("A migration's transform: changes a row in the migration's transaction.")
        .level(Level::Plugin);
    methods
        .add("data.txDelete", |call: Call, p: TxDelete| async move {
            let op = TxOp::Delete {
                collection: p.collection,
                id: parse_id("id", &p.id)?,
            };
            run_op(&call, &p.migration, op).await
        })
        .summary("A migration's transform: deletes a row in the migration's transaction.")
        .level(Level::Plugin);
}

async fn run_op(call: &Call, migration: &str, op: TxOp) -> Result<Value, RpcError> {
    let plugin = match &call.caller {
        Caller::Plugin(p) => p.clone(),
        _ => {
            return Err(RpcError::forbidden(
                "a migration's transform is its plugin's",
            ));
        }
    };
    let tx = {
        let running = call.kernel.facility::<Running>();
        let running = running.migrations.lock().unwrap();
        match running.get(migration) {
            Some((p, tx)) if *p == plugin => tx.clone(),
            _ => {
                return Err(RpcError::not_found(format!(
                    "no migration {migration} of the plugin {plugin} runs"
                )));
            }
        }
    };
    let (reply, rx) = oneshot::channel();
    tx.send((op, reply))
        .await
        .map_err(|_| RpcError::unavailable("the migration has ended"))?;
    rx.await
        .unwrap_or_else(|_| Err(RpcError::unavailable("the migration has ended")))
}

/// Runs transform steps in the extension host and serves their `data.tx*` calls from the migration's
/// transaction until the step returns.
struct HostTransformer<'a> {
    kernel: Kernel,
    plugin: String,
    transforms: &'a dyn Transforms,
}

fn store_error(e: RpcError) -> rookery_store::Error {
    use rookery_proto::ErrorCode as C;
    let m = e.message;
    match e.data.map(|d| d.code) {
        Some(C::Invalid) => rookery_store::Error::Invalid(m),
        Some(C::Forbidden) => rookery_store::Error::Forbidden(m),
        Some(C::NotFound) => rookery_store::Error::NotFound(m),
        Some(C::Conflict) => rookery_store::Error::Conflict(m),
        Some(C::Unavailable) | Some(C::Timeout) => rookery_store::Error::Unavailable(m),
        _ => rookery_store::Error::Internal(m),
    }
}

#[async_trait::async_trait]
impl Transformer for HostTransformer<'_> {
    async fn transform(&self, name: &str, tx: &mut MigrationTx<'_>) -> rookery_store::Result<()> {
        let valid = !name.is_empty()
            && name.len() <= 100
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !valid {
            return Err(rookery_store::Error::invalid(format!(
                "{name:?} is no name of a transform step"
            )));
        }
        let migration = uuid::Uuid::now_v7().to_string();
        let (ops_tx, mut ops) = mpsc::channel::<(TxOp, OpReply)>(16);
        let running = self.kernel.facility::<Running>();
        running
            .migrations
            .lock()
            .unwrap()
            .insert(migration.clone(), (self.plugin.clone(), ops_tx));
        let step = self.transforms.run(&self.plugin, name, &migration);
        tokio::pin!(step);
        let deadline = tokio::time::sleep(TRANSFORM_TIMEOUT);
        tokio::pin!(deadline);
        let result = loop {
            tokio::select! {
                r = &mut step => break r.map_err(store_error),
                _ = &mut deadline => break Err(rookery_store::Error::Timeout(format!("the transform {name} did not finish in time"))),
                Some((op, reply)) = ops.recv() => {
                    let r = match op {
                        TxOp::Rows { collection, after, limit } => {
                            tx.rows(&collection, after, limit).await.map(|rows| json!({ "rows": rows }))
                        }
                        TxOp::Legacy { table, after, limit } => tx
                            .legacy(&table, after.as_deref(), limit)
                            .await
                            .map(|(rows, next)| json!({ "rows": rows, "next": next })),
                        TxOp::Insert { collection, values, space, user } => {
                            tx.insert(&collection, values, space, user).await.map(|row| json!({ "row": row }))
                        }
                        TxOp::Update { collection, id, patch } => {
                            tx.update(&collection, id, patch).await.map(|row| json!({ "row": row }))
                        }
                        TxOp::Delete { collection, id } => tx.delete(&collection, id).await.map(|_| Value::Null),
                    };
                    let _ = reply.send(r.map_err(rpc));
                }
            }
        };
        running.migrations.lock().unwrap().remove(&migration);
        result
    }
}
