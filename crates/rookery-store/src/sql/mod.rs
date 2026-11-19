//! Raw SQL for plugins (docs/platform-split.md, "Raw SQL"; docs/data.md for plugin authors).
//!
//! A plugin that declares `"database": { "sql": true }` gets a Postgres role, a schema of its own and read
//! views over its collections:
//!
//! - [`names`]: the role and schema names, quoting (pure).
//! - [`grants`]: what the role is due and the diff against what it holds (pure).
//! - [`locks`]: the kernel's advisory locks stay the kernel's: no plugin role may execute the lock functions.
//! - [`provision`]: creating, reconciling and revoking the role, schemas, views and grants.
//! - [`views`]: the read views, kept in step with the collection tables.
//! - [`exec`]: running a plugin's SQL on a pool that logs in as the plugin's role.
//! - [`migrations`]: the plugin's `migrations/sql/` files.
//! - [`value`]: parameters in, values out. [`scram`]: password verifiers.
//!
//! # Why connections log in as the role
//!
//! `SET ROLE` is not a boundary: whoever can run `SET ROLE` can run `RESET ROLE`, `SET SESSION
//! AUTHORIZATION` ends at the session's own user, and `DISCARD ALL` resets both. A kernel connection that
//! logged in as the kernel's (privileged) user and ran the plugin's SQL under `SET ROLE` would give the
//! plugin everything the kernel's user has the moment its SQL says `RESET ROLE`. So the kernel keeps one
//! pool per plugin whose connections *authenticate as the plugin's role* with its sealed password: the
//! session's user is the role, there is no more privileged identity to return to, and the role is a member
//! of nothing, so `SET ROLE` to any other role fails. Pooled connections are reset with `DISCARD ALL` when
//! they are returned.

pub mod exec;
pub mod grants;
pub mod headroom;
pub mod locks;
pub mod migrations;
pub mod names;
pub mod provision;
pub mod scram;
pub mod value;
pub(crate) mod views;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sqlx::PgPool;

use crate::Store;
use crate::error::Result;
pub use names::Names;

/// The state raw SQL keeps in the store: pools, open transactions and the deployment's settings for it.
pub(crate) struct State {
    pub tag: Option<String>,
    pub read_url: Option<String>,
    pub service_url: Option<String>,
    pub limits: exec::Limits,
    pub pools: Mutex<HashMap<(String, bool), PgPool>>,
    pub txs: Mutex<HashMap<String, Arc<exec::Tx>>>,
    /// The commands of statements whose plan named a write, by statement text: a statement's command does not
    /// change, so `query` asks the planner once and audits every write it sees.
    pub writes: Mutex<HashMap<String, String>>,
}

impl State {
    pub fn new(
        tag: Option<String>,
        read_url: Option<String>,
        service_url: Option<String>,
        limits: exec::Limits,
    ) -> State {
        State {
            tag,
            read_url,
            service_url,
            limits,
            pools: Mutex::new(HashMap::new()),
            txs: Mutex::new(HashMap::new()),
            writes: Mutex::new(HashMap::new()),
        }
    }
}

/// Raw SQL's operations: [`Store::sql`].
pub struct Sql<'a> {
    pub(crate) store: &'a Store,
}

impl Sql<'_> {
    /// The names of a plugin's role, schemas and views on this deployment.
    pub fn names(&self, plugin: &str) -> Result<Names> {
        Names::new(plugin, self.store.data.sql.tag.as_deref())
    }

    /// The role tag of this deployment, if it has one.
    pub fn tag(&self) -> Option<&str> {
        self.store.data.sql.tag.as_deref()
    }

    /// Closes the plugin's pools and rolls back its open transactions (its credential changed, or it lost
    /// raw SQL).
    pub async fn forget(&self, plugin: &str) {
        let pools: Vec<PgPool> = {
            let mut p = self.store.data.sql.pools.lock().expect("pools lock");
            let keys: Vec<_> = p.keys().filter(|(n, _)| n == plugin).cloned().collect();
            keys.iter().filter_map(|k| p.remove(k)).collect()
        };
        let txs: Vec<Arc<exec::Tx>> = {
            let t = self.store.data.sql.txs.lock().expect("txs lock");
            t.values().filter(|x| x.plugin == plugin).cloned().collect()
        };
        for t in txs {
            t.abort().await;
        }
        for p in pools {
            p.close().await;
        }
    }

    /// Whether the plugin has raw SQL enabled.
    pub async fn is_enabled(&self, plugin: &str) -> Result<bool> {
        let mut c = self.store.pool().acquire().await?;
        views::is_enabled(&mut c, plugin).await
    }
}
