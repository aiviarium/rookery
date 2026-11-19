//! Hosts inside `rookery serve`.
//!
//! The host core of rookery-core keeps its state through its store, which here is the server's tables
//! ([`backend`]), and its locks, which here are leases ([`locks`]). Only the leader holds links to
//! hosts: once it leads, it writes the key ring, the known-hosts files and the plugin folder from the
//! database, then opens a link to each saved host and takes in its channel. Actions run as jobs of the
//! server ([`runner`]). The methods are in [`methods`].

pub mod backend;
pub mod db;
pub mod events;
pub mod locks;
pub mod methods;
pub mod runner;
pub mod serve;

use std::sync::{Arc, OnceLock};

use rookery_core::paths::Paths;
use rookery_jobs::JobHandler;
use rookery_kernel::{Kernel, Methods, RpcError};

/// The resource a host is: `env://NAME`.
pub fn uri(name: &str) -> String {
    format!("env://{name}")
}

/// A host's name from `env://NAME` or the name itself.
pub fn name_of(given: &str) -> &str {
    given.strip_prefix("env://").unwrap_or(given)
}

/// What the hosts need from the server's configuration, which it puts in place before it leads.
#[derive(Clone)]
pub struct Setup {
    pub database_url: String,
    pub secret_key: Option<rookery_store::seal::Key>,
    pub paths: Paths,
    /// How long provisioners may take and how a new machine is waited for, which tests make quick.
    pub provision: fn() -> rookery_core::provision::Options,
}

/// The hosts' facility: the state of the leader, once it is ready.
#[derive(Default)]
pub struct Hosts {
    serve: OnceLock<Arc<serve::Serve>>,
    failed: OnceLock<String>,
}

impl Hosts {
    /// The state of the hosts, or `UNAVAILABLE` while the leader is getting them ready.
    pub fn serve(&self) -> Result<Arc<serve::Serve>, RpcError> {
        if let Some(serve) = self.serve.get() {
            return Ok(serve.clone());
        }
        Err(match self.failed.get() {
            Some(why) => RpcError::unavailable(format!("the hosts could not be set up: {why}")),
            None => RpcError::unavailable(
                "the server is getting its hosts ready; try again in a moment",
            ),
        })
    }

    /// Waits until the hosts are ready, for tests and for the server's start.
    pub async fn ready(&self) -> Result<Arc<serve::Serve>, String> {
        loop {
            if let Some(serve) = self.serve.get() {
                return Ok(serve.clone());
            }
            if let Some(why) = self.failed.get() {
                return Err(why.clone());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
}

/// What rookery-server registers: the methods, and the job handlers it gives rookery-jobs.
pub struct Registration {
    runner: runner::Runner,
}

impl Registration {
    pub fn new() -> Registration {
        Registration {
            runner: runner::Runner::detached(),
        }
    }

    pub fn handlers(&self) -> Vec<Arc<dyn JobHandler>> {
        self.runner.handlers()
    }

    /// Adds the methods, and the start of the hosts when the server leads.
    pub fn register(self, methods: &mut Methods) {
        methods::register(methods);
        let runner = self.runner;
        methods.on_started(move |kernel| start(kernel.clone(), runner.clone()));
    }
}

impl Default for Registration {
    fn default() -> Self {
        Registration::new()
    }
}

pub fn hosts(kernel: &Kernel) -> Arc<Hosts> {
    kernel.facility::<Hosts>()
}

/// Gets the hosts ready on the leader: the store and locks on the database, the files written from
/// it, then the links.
fn start(kernel: Kernel, runner: runner::Runner) {
    let Some(setup) = kernel.get_facility::<Setup>() else {
        tracing::warn!("rookery serve was not given what the hosts need: they stay unavailable");
        let _ = hosts(&kernel).failed.set("no set-up was given".into());
        return;
    };
    let handle = tokio::runtime::Handle::current();
    tokio::spawn(async move {
        let made = {
            let (setup, kernel) = (setup.clone(), kernel.clone());
            tokio::task::spawn_blocking(move || prepare(&setup, kernel, runner, handle))
                .await
                .map_err(|e| e.to_string())
                .and_then(|made| made)
        };
        match made {
            Ok(serve) => {
                serve.refresh_now(true).await;
                let _ = hosts(&kernel).serve.set(serve.clone());
                tracing::info!(
                    hosts = serve.hosts.read().unwrap().len(),
                    "the hosts are ready"
                );
                let daily = serve.clone();
                let stop = kernel.shutdown.clone();
                tokio::spawn(async move {
                    loop {
                        let pruned = daily.clone();
                        if let Ok(days) = tokio::task::spawn_blocking(move || pruned.prune()).await
                        {
                            let cutoff = chrono::Utc::now() - chrono::Duration::days(days as i64);
                            if let Err(e) = daily.runner.kernel().store.jobs().prune(cutoff).await {
                                tracing::warn!(error = %e, "dropping old jobs failed");
                            }
                        }
                        tokio::select! {
                            _ = tokio::time::sleep(std::time::Duration::from_secs(86400)) => {}
                            _ = stop.cancelled() => return,
                        }
                    }
                });
                tokio::spawn(events::forward(kernel.clone(), serve.clone()));
                // What another server or a job changed in the hosts is told as it is seen.
                let watched = serve.clone();
                let stop = kernel.shutdown.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                            _ = stop.cancelled() => return,
                        }
                        let noticed = watched.clone();
                        let _ = tokio::task::spawn_blocking(move || noticed.notice()).await;
                    }
                });
                let stop = kernel.shutdown.clone();
                tokio::spawn(async move {
                    stop.cancelled().await;
                    serve.close();
                });
            }
            Err(why) => {
                tracing::error!(error = %why, "the hosts could not be set up");
                let _ = hosts(&kernel).failed.set(why);
            }
        }
    });
}

fn prepare(
    setup: &Setup,
    kernel: Kernel,
    runner: runner::Runner,
    handle: tokio::runtime::Handle,
) -> Result<Arc<serve::Serve>, String> {
    let db = db::Db::start(&setup.database_url)?;
    let fence = kernel.store.fence();
    let store = Arc::new(rookery_core::store::Store::from_backend(Box::new(
        backend::ServerBackend::new(db.clone(), setup.secret_key.clone(), fence),
    )));
    rookery_core::store::set_shared(Some(store.clone()));
    let renewals = db::Db::start(&setup.database_url)?;
    rookery_core::runner::set_locks(Some(locks::LeaseLocks::new(db, renewals, fence)));
    let paths = setup.paths.clone();
    write_files(&paths, &store);
    runner.attach(paths.clone(), kernel);
    let serve = serve::Serve::with(paths, store, runner.clone(), setup.provision);
    let resumer = serve.clone();
    runner.set_resume(Arc::new(move |name| {
        let _entered = handle.enter();
        resumer.controller_of(name)
    }));
    Ok(serve)
}

/// The files the host core reads, written from the database before any link opens: the plugin
/// folder, the key ring's key files and each host's known-hosts file.
fn write_files(paths: &Paths, store: &rookery_core::store::Store) {
    match rookery_core::plugin_state::restore(paths) {
        Ok(notices) => notices.iter().for_each(|n| tracing::info!("{n}")),
        Err(e) => tracing::warn!(error = %e, "the plugins in the database were not written"),
    }
    match rookery_core::eggs::builtins(paths) {
        Ok(notices) => notices.iter().for_each(|n| tracing::info!("{n}")),
        Err(e) => {
            tracing::warn!(error = %e, "the plugins that come with rookery were not installed")
        }
    }
    use rookery_core::keys::{known, ring};
    for name in ring::names(store).unwrap_or_default() {
        if let Err(e) = ring::file(paths, store, &name, false) {
            tracing::warn!(key = %name, error = %e, "a key of the ring was not written");
        }
    }
    for (place, _) in
        rookery_core::profiles::list(paths, rookery_core::store::OWN).unwrap_or_default()
    {
        if let Err(e) = known::prepare(paths, store, &place) {
            tracing::warn!(host = %place.argument(), error = %e, "a host's known-hosts file was not written");
        }
    }
}
