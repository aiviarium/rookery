//! What a plugin declares of its data, and the seam to the datastore that keeps it.
//!
//! The plugin system reads `contributes.collections`, `database` and the migrations in the target's
//! `migrations/` folder ([`Declared`]). The datastore applies them: it implements [`PluginData`], and
//! the server sets it as the facility [`DataLayer`]. The plugin system calls [`PluginData::apply`] for
//! each valid plugin that declares data, when the server starts leading and whenever the plugin is
//! installed or changes, before its host half starts. A plugin whose data could not be applied is not
//! used, with the datastore's error as the reason.
//!
//! A migration step `{"op": "transform", "name": "N"}` runs `migrations/N.ts` in the extension host.
//! The datastore runs one with [`Transforms::run`], naming the migration it belongs to; the step's
//! `tx` calls `data.txRows`, `data.txLegacy`, `data.txInsert`, `data.txUpdate` and `data.txDelete`
//! with that `migration` in their params, as the plugin, and the datastore serves those methods.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use rookery_kernel::Kernel;
use rookery_proto::RpcError;
use serde_json::{Value, json};

use crate::loaded::Loaded;

/// One numbered migration of `migrations/NNNN.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct Migration {
    /// From the file's name, or its `version`.
    pub version: i64,
    /// The file's name, such as `0002.json`.
    pub file: String,
    /// The file as it is, with `version` set: its `steps` and options such as `onInstall`.
    pub body: Value,
}

/// What a plugin declares of its data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Declared {
    /// `contributes.collections`, as the manifest has them.
    pub collections: Vec<Value>,
    /// `database`: raw SQL in a schema of the plugin's own.
    pub database: Option<Value>,
    /// The numbered migrations, in order.
    pub migrations: Vec<Migration>,
    /// The raw schema's SQL migrations, `migrations/sql/*.sql`, by name in order.
    pub sql: Vec<String>,
}

impl Declared {
    pub fn is_empty(&self) -> bool {
        self.collections.is_empty()
            && self.database.is_none()
            && self.migrations.is_empty()
            && self.sql.is_empty()
    }
}

/// The datastore's side: it makes a plugin's declared collections and runs its migrations that have
/// not run, in one transaction. It is called again for a plugin that did not change, and then changes
/// nothing.
pub trait PluginData: Send + Sync + 'static {
    fn apply<'a>(
        &'a self,
        kernel: &'a Kernel,
        plugin: &'a Loaded,
        declared: &'a Declared,
        transforms: Transforms,
    ) -> BoxFuture<'a, Result<(), RpcError>>;
}

/// The facility a server sets to keep plugins' data.
#[derive(Clone)]
pub struct DataLayer(pub Arc<dyn PluginData>);

/// Runs a plugin's transform steps in the extension host, for the datastore.
#[derive(Clone)]
pub struct Transforms {
    pub(crate) kernel: Kernel,
    pub(crate) plugins: Arc<crate::Plugins>,
    pub(crate) plugin: String,
    pub(crate) dir: PathBuf,
}

impl Transforms {
    pub fn plugin(&self) -> &str {
        &self.plugin
    }

    /// Runs `migrations/<name>.ts` with a `tx` of the migration `migration`, until its function returns
    /// or [`crate::exthost::TRANSFORM_TIMEOUT`] passes, when its process is ended.
    pub async fn run(&self, name: &str, migration: &str) -> Result<(), RpcError> {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(RpcError::invalid(format!(
                "{name:?} is no name of a transform step"
            )));
        }
        if !self
            .dir
            .join("migrations")
            .join(format!("{name}.ts"))
            .is_file()
        {
            return Err(RpcError::invalid(format!(
                "the transform step {name} of {} needs migrations/{name}.ts",
                self.plugin
            )));
        }
        let params =
            json!({"plugin": self.plugin, "dir": self.dir, "name": name, "migration": migration});
        self.plugins
            .host
            .transform(
                &self.kernel,
                self.plugins.clone(),
                &self.plugin,
                params,
                crate::exthost::TRANSFORM_TIMEOUT,
            )
            .await
    }
}

/// Reads what a loaded manifest and its folder declare of data, and what is wrong with it.
pub fn declared(dir: &Path, manifest: &Value) -> Result<Declared, Vec<String>> {
    let mut problems = vec![];
    let collections = manifest
        .pointer("/contributes/collections")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let database = manifest.get("database").cloned();
    let folder = dir.join("migrations");
    let mut migrations = vec![];
    let mut sql = vec![];
    if let Ok(entries) = std::fs::read_dir(&folder) {
        for entry in entries.flatten() {
            let path = entry.path();
            let file = entry.file_name().to_string_lossy().into_owned();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let digits: String = file.chars().take_while(char::is_ascii_digit).collect();
            let body = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()));
            let mut body = match body {
                Ok(body) if body.is_object() => body,
                Ok(_) => {
                    problems.push(format!("migrations/{file} is not a JSON object"));
                    continue;
                }
                Err(e) => {
                    problems.push(format!("migrations/{file} is not valid JSON: {e}"));
                    continue;
                }
            };
            let version = match body.get("version").and_then(Value::as_i64) {
                Some(version) => version,
                None => match digits.parse::<i64>() {
                    Ok(version) => version,
                    Err(_) => {
                        problems.push(format!(
                            "migrations/{file}: name a migration by its number, such as 0002.json, or give it a version"
                        ));
                        continue;
                    }
                },
            };
            body["version"] = json!(version);
            for step in body["steps"].as_array().into_iter().flatten() {
                if step["op"] == "transform" {
                    let name = step["name"].as_str().unwrap_or("");
                    if !folder.join(format!("{name}.ts")).is_file() {
                        problems.push(format!(
                            "migrations/{file}: the transform step {name:?} needs migrations/{name}.ts"
                        ));
                    }
                }
            }
            migrations.push(Migration {
                version,
                file,
                body,
            });
        }
        if let Ok(entries) = std::fs::read_dir(folder.join("sql")) {
            sql = entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| name.ends_with(".sql"))
                .collect();
            sql.sort();
        }
    }
    migrations.sort_by_key(|m| m.version);
    if let Some(pair) = migrations.windows(2).find(|w| w[0].version == w[1].version) {
        problems.push(format!(
            "migrations/{} and migrations/{} have the same version {}",
            pair[0].file, pair[1].file, pair[0].version
        ));
    }
    let raw = database
        .as_ref()
        .and_then(|d| d.get("sql"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !sql.is_empty() && !raw {
        problems.push(format!(
            "migrations/sql has {} file(s), and the manifest has no database.sql: add \"database\": {{\"sql\": true}}",
            sql.len()
        ));
    }
    if problems.is_empty() {
        Ok(Declared {
            collections,
            database,
            migrations,
            sql,
        })
    } else {
        Err(problems)
    }
}
