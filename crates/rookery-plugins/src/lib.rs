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

/// The plugin system's state on a leader.
pub struct Plugins {
    setup: Setup,
    loaded: RwLock<BTreeMap<String, Arc<Loaded>>>,
    rows: RwLock<BTreeMap<String, record::Row>>,
    /// One change of the plugin folder at a time.
    changing: tokio::sync::Mutex<()>,
    pub(crate) host: exthost::ExtHost,
    started: AtomicBool,
}

impl Plugins {
    pub fn new(setup: Setup) -> Plugins {
        Plugins {
            host: exthost::ExtHost::new(setup.host.clone()),
            setup,
            loaded: RwLock::default(),
            rows: RwLock::default(),
            changing: tokio::sync::Mutex::new(()),
            started: AtomicBool::new(false),
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

    /// The shell a plugin contributes: its folder and its entry. The server serves it at `/`.
    pub fn shell(&self) -> Option<(std::path::PathBuf, String)> {
        self.all().into_iter().find_map(|plugin| {
            let entry = plugin.shell()?;
            let file = loaded::inside(&plugin.dir, entry)?;
            Some((file.parent()?.to_path_buf(), entry.to_owned()))
        })
    }

    /// The process id of the running extension host, if one runs.
    pub fn host_pid(&self) -> Option<u32> {
        self.host.running().and_then(|link| link.pid())
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
            installed_at: row.as_ref().map(record::Row::installed_at),
            updated_at: row.as_ref().map(record::Row::updated_at),
        }
    }

    pub fn infos(&self) -> Vec<PluginInfo> {
        self.all().iter().map(|p| self.info(p)).collect()
    }

    /// Reads the plugin folder again, records it and tells clients what changed. Host halves of
    /// plugins that changed or went away stop; those that start with the server start.
    pub async fn reload(self: &Arc<Self>, kernel: &Kernel) -> Result<(), RpcError> {
        let manager = self.manager();
        let found = tokio::task::spawn_blocking(move || {
            manager.scan().iter().map(loaded::load).collect::<Vec<_>>()
        })
        .await
        .map_err(|e| RpcError::internal(format!("reading the plugins: {e}")))?;
        let mut found = with_dependencies(found);
        self.apply_data(kernel, &mut found).await;
        record::sync(&kernel.store, &found).await?;
        let rows = record::rows(&kernel.store).await?;
        let new: BTreeMap<String, Arc<Loaded>> = found
            .into_iter()
            .map(|plugin| (plugin.id.clone(), Arc::new(plugin)))
            .collect();
        let old = std::mem::replace(&mut *self.loaded.write().unwrap(), new.clone());
        *self.rows.write().unwrap() = rows;
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

    /// Tells every person's connection that these plugins changed.
    pub fn changed(&self, kernel: &Kernel, ids: &[String]) {
        for id in ids {
            let message = PluginChanged {
                id: id.clone(),
                plugin: self.get(id).map(|p| self.info(&p)),
            };
            for conn in kernel.conns.all() {
                if matches!(conn.caller(), Caller::User { .. } | Caller::Local) {
                    conn.notify("plugins.changed", &message);
                }
            }
        }
    }

    /// Starts a plugin's host half, and first those of the plugins it depends on. A plugin without one
    /// is active as it is.
    pub async fn activate(self: &Arc<Self>, kernel: &Kernel, id: &str) -> Result<(), RpcError> {
        self.activate_within(kernel, id, 0).await
    }

    fn activate_within<'a>(
        self: &'a Arc<Self>,
        kernel: &'a Kernel,
        id: &'a str,
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
            if depth < 8 {
                for dependency in plugin.dependencies().keys() {
                    if self.get(dependency).is_some_and(|d| d.host().is_some()) {
                        self.activate_within(kernel, dependency, depth + 1).await?;
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
        self.activate(&call.kernel, plugin).await?;
        self.host.invoke(call, plugin, "rpc", method, params).await
    }

    /// Starts the plugin system on a leader: reads the plugins, records them and starts the host halves
    /// that start with the server. It runs once.
    pub async fn start(self: &Arc<Self>, kernel: &Kernel) -> Result<(), RpcError> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.reload(kernel).await
    }

    /// Runs a change of the plugin folder, then reads it again.
    pub async fn change<T: Send + 'static>(
        self: &Arc<Self>,
        kernel: &Kernel,
        change: impl FnOnce(&rookery_core::eggs::Manager) -> rookery_core::Result<T> + Send + 'static,
    ) -> Result<T, RpcError> {
        let _one = self.changing.lock().await;
        let manager = self.manager();
        let done = tokio::task::spawn_blocking(move || change(&manager).map_err(|e| e.to_string()))
            .await
            .map_err(|e| RpcError::internal(format!("changing the plugins: {e}")))?;
        // What was changed before a failure is read too.
        self.reload(kernel).await?;
        let done = done.map_err(RpcError::invalid)?;
        Ok(done)
    }

    /// Runs something of the plugin folder that changes nothing, such as asking for updates.
    pub async fn look<T: Send + 'static>(
        &self,
        look: impl FnOnce(&rookery_core::eggs::Manager) -> rookery_core::Result<T> + Send + 'static,
    ) -> Result<T, RpcError> {
        let manager = self.manager();
        tokio::task::spawn_blocking(move || look(&manager).map_err(|e| e.to_string()))
            .await
            .map_err(|e| RpcError::internal(format!("reading the plugins: {e}")))?
            .map_err(RpcError::invalid)
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
