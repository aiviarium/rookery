//! `rookery serve` on Postgres.
//!
//! The server listens at once. One server per database leads: the one that holds the Postgres advisory
//! lock. Until a server has it, it is a standby: `/healthz` and everything else answer 503, and it waits
//! on the lock. Once it leads, it migrates the database, builds the kernel and serves `/ws`, `/rpc`,
//! `/auth/*` and the shell. A leader that loses the lock, or cannot confirm it holds it, stops leading at
//! once and stands by again. The CLI on the same machine reaches it on a Unix socket as an admin, with no
//! token, by the socket's file permissions.

pub mod client;
pub mod config;
pub mod edge;
pub mod leader;
pub mod rpc;
pub mod shell;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use rookery_jobs::JobHandler;
use rookery_kernel::{Kernel, Methods};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub use config::ServerConfig;
pub use leader::Role;
pub use rookery_store;

/// Every method of the server.
pub fn methods() -> Methods {
    methods_with(Vec::new(), rookery_jobs::Options::default())
}

/// Every method of the server, with these handlers of jobs.
pub fn methods_with(
    mut handlers: Vec<Arc<dyn JobHandler>>,
    jobs: rookery_jobs::Options,
) -> Methods {
    let mut methods = Methods::default();
    rookery_kernel::streams::register(&mut methods);
    rookery_auth::register(&mut methods);
    rookery_facilities::register(&mut methods);
    let hosts = rookery_hosts::Registration::new();
    handlers.extend(hosts.handlers());
    rookery_jobs::register(&mut methods, handlers, jobs);
    hosts.register(&mut methods);
    rookery_plugins::register(&mut methods);
    methods
}

/// Seals every secret and credential again under `new`, on the database of `config`, whose key is the
/// old one. It refuses while a server leads, which would keep using the old key.
pub async fn rekey(
    config: &ServerConfig,
    new: &rookery_store::seal::Key,
) -> Result<rookery_store::secrets::Rekeyed, String> {
    let old = config.secret_key.clone().ok_or(
        "the old key is ROOKERY_SECRET_KEY or ROOKERY_SECRET_KEY_FILE, and neither is set",
    )?;
    let lock = leader::Lock::try_acquire(&config.lock_url).await?.ok_or(
        "a rookery serve runs on this database: stop it first, and start it again with the new key",
    )?;
    let store = rookery_store::Store::open(rookery_store::StoreOptions::new(&config.database_url))
        .await
        .map_err(|e| format!("opening the database: {e}"))?;
    let done = store
        .secrets()
        .rekey(&old, new)
        .await
        .map_err(|e| e.to_string());
    store.close().await;
    drop(lock);
    done
}

/// A server that is running.
pub struct Running {
    /// Where it listens over TCP.
    pub addr: SocketAddr,
    pub socket: Option<PathBuf>,
    role: watch::Receiver<Role>,
    edge: Arc<edge::Edge>,
    stop: CancellationToken,
    done: tokio::task::JoinHandle<Result<(), String>>,
}

impl Running {
    pub fn role(&self) -> Role {
        *self.role.borrow()
    }

    /// Waits until the server leads.
    pub async fn leading(&mut self) {
        let _ = self.role.wait_for(|r| *r == Role::Leader).await;
    }

    /// Waits until the server does not lead.
    pub async fn standing_by(&mut self) {
        let _ = self.role.wait_for(|r| *r == Role::Standby).await;
    }

    /// The leader's epoch this server's writes name, while it leads.
    pub fn epoch(&self) -> Option<i64> {
        self.edge.kernel().and_then(|k| k.store.fence())
    }

    /// Stops the server and waits for it.
    pub async fn stop(self) -> Result<(), String> {
        self.stop.cancel();
        self.done.await.map_err(|e| e.to_string())?
    }
}

/// Opens the store once the server holds the lock, which migrates the database.
async fn open_store(config: &ServerConfig) -> Result<rookery_store::Store, String> {
    let mut options = rookery_store::StoreOptions::new(&config.database_url);
    options.secret_key = config.secret_key.clone();
    options.blobs = config.blobs.clone();
    // The tables of hosts are collections of Rookery's too, read through `data.*` and the change log.
    options.system_tables = rookery_hosts::backend::system_tables();
    if config.secret_key.is_none() {
        tracing::warn!(
            "no secret key is set: secrets and the credentials of connections are refused until ROOKERY_SECRET_KEY or ROOKERY_SECRET_KEY_FILE is"
        );
    }
    rookery_store::Store::open(options)
        .await
        .map_err(|e| format!("opening the database: {e}"))
}

/// Builds the kernel of a leader on its fenced store and starts the facilities.
fn lead(config: &ServerConfig, store: rookery_store::Store, methods: Methods) -> Kernel {
    let mut kernel_config = rookery_kernel::Config::new(&config.public_url);
    kernel_config.auth = config.auth;
    kernel_config.oidc = config.oidc.clone();
    kernel_config.admins = config.admins.clone();
    kernel_config.outbound_allow = config.outbound_allow.clone();
    let kernel = Kernel::new(kernel_config, store, methods);
    if let Some(ports) = config
        .ports_url
        .as_deref()
        .and_then(|url| rookery_hosts::proxy::Ports::parse(url).ok())
    {
        kernel.set_facility(ports);
    }
    if let Some(paths) = &config.paths {
        kernel.set_facility(rookery_hosts::Setup {
            database_url: config.database_url.clone(),
            secret_key: config.secret_key.clone(),
            paths: paths.clone(),
            provision: config.provision,
        });
        kernel.set_facility(rookery_plugins::Setup {
            paths: paths.clone(),
            host: config.exthost.clone(),
        });
    }
    kernel.methods.started(&kernel);
    kernel
}

/// Starts a server: it listens at once, and leads when it gets the lock.
pub async fn start(config: ServerConfig) -> Result<Running, String> {
    start_with(config, methods()).await
}

/// [`start`] with methods of its own, such as with handlers of jobs.
pub async fn start_with(mut config: ServerConfig, methods: Methods) -> Result<Running, String> {
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|e| format!("listening on {}: {e}", config.listen))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    // A server told to listen on port 0 is reached where it ended up.
    if config.public_url.is_empty() {
        config.public_url = format!("http://{addr}");
    }
    let socket = match &config.socket {
        Some(path) => Some((path.clone(), edge::bind_socket(path)?)),
        None => None,
    };
    let socket_path = socket.as_ref().map(|(path, _)| path.clone());
    let edge = Arc::new(edge::Edge::new(config.shell_dir.clone()));
    let (role_tx, role) = watch::channel(Role::Standby);
    let stop = CancellationToken::new();
    let done = tokio::spawn(run(
        config,
        methods,
        listener,
        socket,
        edge.clone(),
        role_tx,
        stop.clone(),
    ));
    Ok(Running {
        addr,
        socket: socket_path,
        role,
        edge,
        stop,
        done,
    })
}

/// How long the host halves of plugins have to run their cleanups when the server stops leading.
const PLUGINS_STOP: std::time::Duration = std::time::Duration::from_secs(5);

/// Stops the host halves of plugins, which run their cleanups while the store is still open.
async fn stop_plugins(kernel: &Kernel) {
    if let Some(plugins) = kernel.get_facility::<rookery_plugins::Plugins>() {
        plugins.stop(kernel, PLUGINS_STOP).await;
    }
}

/// How long a deposed leader waits for its store to close before it stands by again.
const CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

async fn run(
    config: ServerConfig,
    methods: Methods,
    listener: tokio::net::TcpListener,
    socket: Option<(PathBuf, tokio::net::UnixListener)>,
    edge: Arc<edge::Edge>,
    role: watch::Sender<Role>,
    stop: CancellationToken,
) -> Result<(), String> {
    let quit = CancellationToken::new();
    let tcp = tokio::spawn(edge::serve_tcp(edge.clone(), listener, quit.clone()));
    let unix = socket.map(|(path, listener)| {
        tokio::spawn(edge::serve_socket(
            edge.clone(),
            path,
            listener,
            quit.clone(),
        ))
    });
    let result = loop {
        // A standby: it waits for the lock.
        let mut lock = tokio::select! {
            _ = stop.cancelled() => break Ok(None),
            lock = leader::Lock::acquire(&config.lock_url) => match lock {
                Ok(lock) => lock,
                Err(e) => break Err(e),
            },
        };
        let store = tokio::select! {
            biased;
            _ = stop.cancelled() => break Ok(Some(lock)),
            lost = lock.lost() => {
                tracing::warn!(reason = %lost, "the leader lock was lost while starting to lead; this server stands by");
                continue;
            }
            store = open_store(&config) => match store {
                Ok(store) => store,
                Err(e) => break Err(e),
            },
        };
        let epoch = match lock.take_epoch().await {
            Ok(epoch) => epoch,
            Err(e) => {
                tracing::warn!(error = %e, "this server took the lock but could not start its epoch; it stands by");
                store.close().await;
                continue;
            }
        };
        let kernel = lead(&config, store.fenced(epoch), methods.clone());
        tracing::info!(epoch, "this server holds the lock and leads");
        edge.set_kernel(kernel.clone());
        let _ = role.send(Role::Leader);

        let lost = tokio::select! {
            _ = stop.cancelled() => None,
            lost = lock.lost() => Some(lost),
        };
        let Some(lost) = lost else {
            break Ok(Some(lock));
        };
        // Stop leading at once: answer 503, stop the workers, the links and the connections. What the
        // kernel still writes is refused by the epoch.
        tracing::error!(epoch, reason = %lost, "this server stops leading and stands by");
        if let Some(kernel) = edge.clear_kernel() {
            stop_plugins(&kernel).await;
            kernel.shutdown.cancel();
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, kernel.store.close()).await;
        }
        let _ = role.send(Role::Standby);
        drop(lock);
    };
    // A graceful stop drains requests before another server can take over.
    let kernel = edge.kernel();
    if let Some(kernel) = &kernel {
        // The edge stops answering first, so no request starts a host half once they are stopped.
        edge.clear_kernel();
        stop_plugins(kernel).await;
        kernel.shutdown.cancel();
    }
    quit.cancel();
    let _ = tcp.await;
    if let Some(unix) = unix {
        let _ = unix.await;
    }
    edge.clear_kernel();
    if let Some(kernel) = kernel {
        kernel.store.close().await;
    }
    let _ = role.send(Role::Standby);
    result.map(drop)
}

/// `rookery serve`: runs until SIGINT or SIGTERM, or until the database cannot be reached.
pub async fn serve(config: ServerConfig) -> Result<(), String> {
    let _ = tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
    let running = start(config).await?;
    tracing::info!(addr = %running.addr, "rookery serve listens; it leads once it holds the database's lock");
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| e.to_string())?;
    let Running { stop, mut done, .. } = running;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => stop.cancel(),
        _ = terminate.recv() => stop.cancel(),
        ended = &mut done => return ended.map_err(|e| e.to_string())?,
    }
    done.await.map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    #[test]
    fn every_method_has_a_level() {
        let methods = super::methods();
        assert_eq!(
            methods.without_level(),
            Vec::<&str>::new(),
            "these methods have no level, so nobody may call them"
        );
    }

    /// `api/methods.json` lists every method, its level and its params. `ROOKERY_UPDATE_DOCS=1` writes it.
    #[test]
    fn the_method_report_is_current() {
        let report = serde_json::to_string_pretty(&super::methods().report()).unwrap() + "\n";
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../api/methods.json");
        if std::env::var_os("ROOKERY_UPDATE_DOCS").is_some() {
            std::fs::write(&path, &report).unwrap();
        }
        let kept = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            kept == report,
            "api/methods.json is stale: ROOKERY_UPDATE_DOCS=1 cargo test -p rookery-server"
        );
    }
}
