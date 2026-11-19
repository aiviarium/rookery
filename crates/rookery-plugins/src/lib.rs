//! The plugin system of `rookery serve`.
//!
//! A plugin is a Hatchery package with a `rookery` target, whose `plugin.json` says what it is and what
//! it contributes: host features and provisioners, a host half, a client half, a shell and the rest. A
//! target that still has a `rookery.json` is read as a `plugin.json` with host features and
//! provisioners alone, for one release.
//!
//! - [`register`] adds `plugins.*`. The server sets a [`Setup`] as a facility of the kernel before it
//!   starts it, and mounts [`routes`], which serve the client halves.
//! - Installs, updates and removals go through the `hatchery` library into the server's plugin folder
//!   (rookery-core's `eggs`), and each is recorded in `rookery.plugins` ([`record`]).
//! - Host halves run in the extension host ([`exthost`]), a Bun process started when the first one is
//!   needed.

pub mod client;
pub mod data;
pub mod exthost;
pub mod jobs;
pub mod loaded;
pub mod methods;
pub mod record;
pub mod wire;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use rookery_core::paths::Paths;
use rookery_kernel::{Caller, Kernel};
use rookery_proto::{ErrorBody, ErrorCode, RpcError};

pub use client::routes;
pub use data::{DataLayer, Declared, PluginData, Transforms};
pub use exthost::HostOptions;
pub use loaded::Loaded;
pub use methods::register;
use wire::{PluginChanged, PluginInfo, PluginState};

/// What the server gives the plugin system.
#[derive(Debug, Clone)]
pub struct Setup {
    /// The server's folders. Plugins are installed in `plugins/` of its configuration folder, from the
    /// registries in its `hatchery.kdl` and the one of Rookery's provisioning source.
    pub paths: Paths,
    pub host: HostOptions,
}

/// How long a read waits for the plugin folder before it says the server is not ready, rather than
/// answer as if no plugin were installed.
const READY_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// The plugin system's state on a leader.
pub struct Plugins {
    setup: Setup,
    loaded: RwLock<BTreeMap<String, Arc<Loaded>>>,
    rows: RwLock<BTreeMap<String, record::Row>>,
    /// Plugins recorded as installed that the folder does not have, as the last read found.
    missing: RwLock<Vec<String>>,
    /// The jobs the plugins declare, by kind.
    jobs: RwLock<BTreeMap<String, jobs::DeclaredJob>>,
    /// One change of the plugin folder at a time.
    changing: tokio::sync::Mutex<()>,
    pub(crate) host: exthost::ExtHost,
    started: AtomicBool,
    /// The folder was read, as a new leader reads it before anything else uses the plugins.
    ready: std::sync::OnceLock<()>,
    /// Why the folder could not be read, when it could not.
    failed: std::sync::OnceLock<String>,
}

impl Plugins {
    pub fn new(setup: Setup) -> Plugins {
        Plugins {
            host: exthost::ExtHost::new(setup.host.clone()),
            setup,
            loaded: RwLock::default(),
            rows: RwLock::default(),
            missing: RwLock::default(),
            jobs: RwLock::default(),
            changing: tokio::sync::Mutex::new(()),
            started: AtomicBool::new(false),
            ready: std::sync::OnceLock::new(),
            failed: std::sync::OnceLock::new(),
        }
    }

    pub fn setup(&self) -> &Setup {
        &self.setup
    }

    fn manager(&self) -> rookery_core::eggs::Manager {
        rookery_core::eggs::Manager::new(&self.setup.paths)
    }

    pub fn get(&self, id: &str) -> Option<Arc<Loaded>> {
        self.loaded.read().unwrap().get(id).cloned()
    }

    pub fn all(&self) -> Vec<Arc<Loaded>> {
        self.loaded.read().unwrap().values().cloned().collect()
    }

    /// Waits until the plugin folder was read, as the leader does before it answers, so that a request
    /// that comes just after a start or a failover does not see a server with no plugins.
    pub async fn ready(&self) -> Result<(), RpcError> {
        loop {
            if self.ready.get().is_some() {
                return Ok(());
            }
            if let Some(why) = self.failed.get() {
                return Err(RpcError::unavailable(format!(
                    "the plugins could not be read: {why}"
                )));
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// [`Plugins::ready`] with a short bound, for a request that answers rather than wait.
    pub async fn ready_soon(&self) -> Result<(), RpcError> {
        match tokio::time::timeout(READY_WAIT, self.ready()).await {
            Ok(ready) => ready,
            Err(_) => Err(RpcError::unavailable(
                "the server is reading its plugins; try again in a moment",
            )),
        }
    }

    /// The shell a plugin contributes: its folder and its entry. The server serves it at `/`.
    ///
    /// A plugin the server does not use has no shell served: its manifest may name one that its files,
    /// or the data it declares, do not bear out.
    pub fn shell(&self) -> Option<(std::path::PathBuf, String)> {
        self.all().into_iter().find_map(|plugin| {
            if !plugin.valid() {
                return None;
            }
            let shell = plugin.shell.as_ref()?;
            Some((shell.root.clone(), shell.entry.clone()))
        })
    }

    /// The process id of a plugin's extension host, if one runs.
    pub fn host_pid(&self, plugin: &str) -> Option<u32> {
        self.host.running(plugin).and_then(|link| link.pid())
    }

    /// A plugin as `plugins.list` says it.
    pub fn info(&self, plugin: &Loaded) -> PluginInfo {
        let (state, error) = if let Some(problem) = &plugin.problem {
            (PluginState::Invalid, Some(problem.clone()))
        } else if plugin.host().is_none() {
            (PluginState::Active, None)
        } else {
            match self.host.state(&plugin.id) {
                (Some(state), error) => (state, error),
                (None, _) => (PluginState::Inactive, None),
            }
        };
        let row = self.rows.read().unwrap().get(&plugin.id).cloned();
        PluginInfo {
            id: plugin.id.clone(),
            state,
            error,
            manifest: plugin.manifest.clone(),
            client: plugin
                .valid()
                .then(|| plugin.client.as_ref().map(|c| c.info(&plugin.id)))
                .flatten(),
            package: plugin.package.clone(),
            version: plugin.version.clone(),
            origin: loaded::origin_name(plugin.origin).into(),
            source: plugin.source.clone(),
            legacy: plugin.format == Some(rookery_api::plugin_contract::Format::Legacy),
            label: plugin.core.label.clone(),
            description: plugin.core.description.clone(),
            dir: plugin.core.dir.clone(),
            problem: plugin
                .problem
                .as_ref()
                .map(|p| p.message.clone())
                .unwrap_or_default(),
            features: plugin.core.features.clone(),
            provisioners: plugin.core.provisioners.clone(),
            installed_at: row.as_ref().map(record::Row::installed_at),
            updated_at: row.as_ref().map(record::Row::updated_at),
        }
    }

    /// Plugins recorded as installed that the folder does not have.
    pub fn missing(&self) -> Vec<String> {
        self.missing.read().unwrap().clone()
    }

    /// A plugin as a caller sees it: where its package is on the server and where it came from are for
    /// an admin alone.
    pub fn info_for(&self, plugin: &Loaded, admin: bool) -> PluginInfo {
        let mut info = self.info(plugin);
        if !admin {
            info.dir.clear();
            info.source.clear();
        }
        info
    }

    pub fn info_of(&self, id: &str) -> Option<PluginInfo> {
        self.get(id).map(|p| self.info(&p))
    }

    pub fn infos(&self) -> Vec<PluginInfo> {
        self.all().iter().map(|p| self.info(p)).collect()
    }

    /// [`Plugins::infos`] as a caller sees them. Missing plugins are the server's folder against the
    /// database, which is an admin's to know.
    pub fn infos_for(&self, admin: bool) -> Vec<PluginInfo> {
        self.all().iter().map(|p| self.info_for(p, admin)).collect()
    }

    /// Reads the plugin folder again, records it and tells clients what changed. Host halves of
    /// plugins that changed or went away stop; those that start with the server start.
    pub async fn reload(self: &Arc<Self>, kernel: &Kernel) -> Result<(), RpcError> {
        self.ready().await?;
        let _one = self.changing.lock().await;
        self.reload_held(kernel, &[]).await
    }

    /// [`Plugins::reload`], for a caller that holds `changing`. The records of `removed`, the plugins
    /// a change took out, go too.
    async fn reload_held(
        self: &Arc<Self>,
        kernel: &Kernel,
        removed: &[String],
    ) -> Result<(), RpcError> {
        let manager = self.manager();
        let found = tokio::task::spawn_blocking(move || {
            manager.scan().iter().map(loaded::load).collect::<Vec<_>>()
        })
        .await
        .map_err(|e| RpcError::internal(format!("reading the plugins: {e}")))?;
        let mut found = with_dependencies(found);
        self.apply_data(kernel, &mut found).await;
        let missing = record::sync(&kernel.store, &found, removed).await?;
        if !missing.is_empty() {
            tracing::warn!(
                ?missing,
                "plugins are recorded as installed and are not in the plugin folder"
            );
        }
        *self.missing.write().unwrap() = missing;
        let rows = record::rows(&kernel.store).await?;
        let new: BTreeMap<String, Arc<Loaded>> = found
            .into_iter()
            .map(|plugin| (plugin.id.clone(), Arc::new(plugin)))
            .collect();
        let old = std::mem::replace(&mut *self.loaded.write().unwrap(), new.clone());
        *self.rows.write().unwrap() = rows;
        let declared: BTreeMap<String, jobs::DeclaredJob> = new
            .values()
            .flat_map(|plugin| jobs::declared(plugin))
            .map(|job| (job.kind.clone(), job))
            .collect();
        let before: Vec<jobs::DeclaredJob> = self.jobs.read().unwrap().values().cloned().collect();
        let now: Vec<jobs::DeclaredJob> = declared.values().cloned().collect();
        jobs::sync(kernel, self, &before, &now);
        *self.jobs.write().unwrap() = declared;
        let mut changed = vec![];
        for (id, before) in &old {
            match new.get(id) {
                Some(after) if after == before => {}
                _ => {
                    self.host.deactivate(kernel, id).await;
                    changed.push(id.clone());
                }
            }
        }
        changed.extend(new.keys().filter(|id| !old.contains_key(*id)).cloned());
        self.changed(kernel, &changed);
        for plugin in new.values() {
            if plugin.valid() && plugin.host().is_some() && plugin.starts_at_once() {
                let (kernel, plugins, id) = (kernel.clone(), self.clone(), plugin.id.clone());
                tokio::spawn(async move {
                    let _ = plugins.activate(&kernel, &id).await;
                });
            }
        }
        Ok(())
    }

    /// Has the datastore apply what each valid plugin declares of its data, when it is new or changed.
    /// One whose data could not be applied is not used.
    async fn apply_data(self: &Arc<Self>, kernel: &Kernel, found: &mut [Loaded]) {
        let layer = kernel.get_facility::<data::DataLayer>();
        for plugin in found.iter_mut() {
            if !plugin.valid() || plugin.declared.is_empty() {
                continue;
            }
            if self.get(&plugin.id).is_some_and(|old| *old == *plugin) {
                continue;
            }
            let done = match &layer {
                None => Err(RpcError::unavailable(format!(
                    "{} declares collections, a database or migrations, and this server has no datastore for them",
                    plugin.id
                ))),
                Some(layer) => {
                    let transforms = data::Transforms {
                        kernel: kernel.clone(),
                        plugins: self.clone(),
                        plugin: plugin.id.clone(),
                        dir: plugin.dir.clone(),
                    };
                    layer
                        .0
                        .apply(kernel, plugin, &plugin.declared, transforms)
                        .await
                }
            };
            if let Err(e) = done {
                tracing::warn!(plugin = %plugin.id, error = %e, "a plugin's data was not applied");
                plugin.problem = Some(ErrorBody {
                    code: e.error_code(),
                    message: format!("the data of {} was not applied: {}", plugin.id, e.message),
                    details: e.details().cloned(),
                });
            }
        }
    }

    /// Tells every person's connection that these plugins changed. What a subscriber that is not an
    /// admin is told leaves out where a plugin's package is and where it came from.
    pub fn changed(&self, kernel: &Kernel, ids: &[String]) {
        for id in ids {
            let plugin = self.get(id);
            for conn in kernel.conns.all() {
                if !matches!(conn.caller(), Caller::User { .. } | Caller::Local) {
                    continue;
                }
                let admin = conn.caller().is_admin();
                let message = PluginChanged {
                    id: id.clone(),
                    plugin: plugin.as_ref().map(|p| self.info_for(p, admin)),
                };
                conn.notify("plugins.changed", &message);
            }
        }
    }

    /// Starts a plugin's host half, and first those of the plugins it depends on. A plugin without one
    /// is active as it is.
    /// This is the server's own doing, as at startup or after a crash, which may start one that failed.
    pub async fn activate(self: &Arc<Self>, kernel: &Kernel, id: &str) -> Result<(), RpcError> {
        self.activate_within(kernel, id, true, 0).await
    }

    /// [`Plugins::activate`] for a caller: a plugin that failed, or one it depends on that failed,
    /// starts again for an admin alone.
    pub async fn activate_as(
        self: &Arc<Self>,
        kernel: &Kernel,
        id: &str,
        admin: bool,
    ) -> Result<(), RpcError> {
        self.activate_within(kernel, id, admin, 0).await
    }

    fn activate_within<'a>(
        self: &'a Arc<Self>,
        kernel: &'a Kernel,
        id: &'a str,
        admin: bool,
        depth: usize,
    ) -> futures_util::future::BoxFuture<'a, Result<(), RpcError>> {
        Box::pin(async move {
            let plugin = self
                .get(id)
                .ok_or_else(|| RpcError::not_found(format!("no plugin {id} is installed")))?;
            if let Some(problem) = &plugin.problem {
                return Err(error_of(problem));
            }
            let Some(manifest) = &plugin.manifest else {
                return Err(RpcError::invalid(format!("{id} has no manifest")));
            };
            if plugin.host().is_none() {
                return Ok(());
            }
            if let (Some(PluginState::Failed), error) = self.host.state(id)
                && !admin
            {
                let said = error.map(|e| e.message).unwrap_or_default();
                return Err(RpcError::unavailable(format!(
                    "{id} failed, and an admin starts it again: {said}"
                )));
            }
            if depth < 8 {
                for dependency in plugin.dependencies().keys() {
                    if self.get(dependency).is_some_and(|d| d.host().is_some()) {
                        self.activate_within(kernel, dependency, admin, depth + 1)
                            .await?;
                    }
                }
            }
            let done = self
                .host
                .activate(kernel, self.clone(), id, &plugin.dir, manifest)
                .await;
            self.changed(kernel, &[id.to_owned()]);
            done
        })
    }

    /// Runs a method a plugin's host half registered with `rookery.rpc.handle`, starting it first.
    pub async fn rpc(
        self: &Arc<Self>,
        call: &rookery_kernel::Call,
        plugin: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        self.activate_as(&call.kernel, plugin, call.is_admin())
            .await?;
        self.host
            .invoke(call, self, plugin, "rpc", method, params)
            .await
    }

    /// Stops every host half when the server stops, running their cleanups within `limit` in all. The
    /// server awaits it before it closes the store.
    pub async fn stop(&self, kernel: &Kernel, limit: std::time::Duration) {
        self.host.stop_all(kernel, limit).await;
    }

    /// Waits until the hosts have put the plugin folder in place from the database, as a new leader
    /// does, so that nothing reads or changes it half written. Nothing to wait for without hosts.
    async fn folder_ready(kernel: &Kernel) {
        // The hosts' facility is made when they start, which may come after this.
        if kernel.get_facility::<rookery_hosts::Setup>().is_some() {
            let _ = kernel.facility::<rookery_hosts::Hosts>().ready().await;
        }
    }

    /// Starts the plugin system on a leader: reads the plugins, records them and starts the host halves
    /// that start with the server. It runs once, and sets the ready signal the rest waits on.
    pub async fn start(self: &Arc<Self>, kernel: &Kernel) -> Result<(), RpcError> {
        if self.started.swap(true, Ordering::SeqCst) {
            self.ready().await?;
            return Ok(());
        }
        let plugins = Arc::downgrade(self);
        rookery_jobs::set_enqueue_policy(
            kernel,
            Arc::new(move |call, kind| {
                let plugins = plugins.upgrade()?;
                let jobs = plugins.jobs.read().unwrap();
                jobs::may_enqueue(&jobs, call, kind)
            }),
        );
        let done = async {
            Self::folder_ready(kernel).await;
            // The folder is read under the change lock, before anything else may use the plugins.
            let _one = self.changing.lock().await;
            self.reload_held(kernel, &[]).await
        }
        .await;
        match &done {
            Ok(()) => {
                let _ = self.ready.set(());
            }
            Err(e) => {
                let _ = self.failed.set(e.message.clone());
            }
        }
        done
    }

    /// Runs a change of the plugin folder, then reads it again, and has the hosts read it too. Gives
    /// what the change said, and the saved hosts that are behind what is installed now.
    pub async fn change<T: Send + 'static>(
        self: &Arc<Self>,
        kernel: &Kernel,
        change: impl FnOnce(&rookery_core::eggs::Manager) -> rookery_core::Result<T> + Send + 'static,
    ) -> Result<(T, Vec<rookery_api::HostBehind>), RpcError> {
        self.ready().await?;
        Self::folder_ready(kernel).await;
        let _one = self.changing.lock().await;
        let manager = self.manager();
        let held: BTreeMap<String, String> = self
            .all()
            .iter()
            .map(|p| (p.id.clone(), p.package.clone()))
            .collect();
        let held_ids: Vec<String> = held.keys().cloned().collect();
        let done = tokio::task::spawn_blocking(move || {
            let done = change(&manager).map_err(|e| e.to_string());
            match refuse_taken_ids(&manager, &held) {
                Ok(()) => done,
                Err(refused) => Err(refused),
            }
        })
        .await
        .map_err(|e| RpcError::internal(format!("changing the plugins: {e}")))?;
        // What was changed before a failure is read too; what it took out loses its record.
        let after: Vec<String> = {
            let manager = self.manager();
            tokio::task::spawn_blocking(move || manager.scan().into_iter().map(|p| p.id).collect())
                .await
                .unwrap_or_default()
        };
        let removed: Vec<String> = held_ids
            .into_iter()
            .filter(|id| !after.contains(id))
            .collect();
        self.reload_held(kernel, &removed).await?;
        let behind = rookery_hosts::plugins_changed(kernel).await;
        let done = done.map_err(RpcError::invalid)?;
        Ok((done, behind))
    }

    /// Runs something of the plugin folder that changes nothing, such as asking for updates.
    pub async fn look<T: Send + 'static>(
        &self,
        look: impl FnOnce(&rookery_core::eggs::Manager) -> rookery_core::Result<T> + Send + 'static,
    ) -> Result<T, RpcError> {
        self.ready().await?;
        let manager = self.manager();
        tokio::task::spawn_blocking(move || look(&manager).map_err(|e| e.to_string()))
            .await
            .map_err(|e| RpcError::internal(format!("reading the plugins: {e}")))?
            .map_err(RpcError::invalid)
    }
}

/// Takes out each package that came in under a plugin id another package holds: a plugin's settings,
/// secrets and data are kept by its id, and would go to the newcomer.
fn refuse_taken_ids(
    manager: &rookery_core::eggs::Manager,
    held: &BTreeMap<String, String>,
) -> Result<(), String> {
    let mut by_id: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for package in manager.locked() {
        let id = package.rsplit('/').next().unwrap_or(&package).to_owned();
        by_id.entry(id).or_default().push(package);
    }
    let mut refused = vec![];
    for (id, packages) in by_id {
        if packages.len() < 2 {
            continue;
        }
        let keeper = held
            .get(&id)
            .filter(|holder| packages.contains(holder))
            .cloned()
            .unwrap_or_else(|| packages[0].clone());
        for newcomer in packages.into_iter().filter(|p| *p != keeper) {
            if let Err(e) = manager.remove_package(&newcomer) {
                tracing::error!(package = %newcomer, error = %e, "a package under a taken plugin id could not be removed");
            }
            refused.push(format!(
                "the plugin id {id} is held by the package {keeper}, so {newcomer} was not installed: remove {keeper} first to put another package in its place"
            ));
        }
    }
    if refused.is_empty() {
        Ok(())
    } else {
        Err(refused.join("; "))
    }
}

fn error_of(body: &ErrorBody) -> RpcError {
    let error = RpcError::new(body.code, body.message.clone());
    match &body.details {
        Some(details) => error.with_details(details.clone()),
        None => error,
    }
}

/// Marks invalid each plugin whose dependencies are missing, out of range or invalid themselves.
fn with_dependencies(mut found: Vec<Loaded>) -> Vec<Loaded> {
    loop {
        let versions: BTreeMap<String, Option<String>> = found
            .iter()
            .map(|p| (p.id.clone(), p.valid().then(|| p.version.clone())))
            .collect();
        let mut marked = false;
        for plugin in found.iter_mut().filter(|p| p.valid()) {
            let mut problems = vec![];
            for (id, range) in plugin.dependencies() {
                let accepts = hatchery::semver::VersionReq::parse(&range).ok();
                match versions.get(&id) {
                    None => problems.push(format!(
                        "it needs the plugin {id} {range}, which is not installed"
                    )),
                    Some(None) => {
                        problems.push(format!("it needs the plugin {id}, which cannot be used"))
                    }
                    Some(Some(version)) => {
                        let fits = hatchery::semver::Version::parse(version)
                            .ok()
                            .zip(accepts.as_ref())
                            .is_some_and(|(v, req)| req.matches(&v));
                        if !fits {
                            problems.push(format!(
                                "it needs the plugin {id} {range}, and {version} is installed"
                            ));
                        }
                    }
                }
            }
            if !problems.is_empty() {
                plugin.problem = Some(ErrorBody {
                    code: ErrorCode::Unavailable,
                    message: format!("{} cannot be used: {}", plugin.id, problems.join("; ")),
                    details: Some(serde_json::json!({ "problems": problems })),
                });
                marked = true;
            }
        }
        if !marked {
            return found;
        }
    }
}
