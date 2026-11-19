//! `rookery serve` on Postgres.
//!
//! The server listens at once. One server per database leads: the one that holds the Postgres advisory
//! lock. Until a server has it, it is a standby: `/healthz` and everything else answer 503, and it waits
//! on the lock. Once it leads, it migrates the database, builds the kernel and serves `/ws`, `/rpc`,
//! `/auth/*` and the shell. The CLI on the same machine reaches it on a Unix socket as an admin, with no
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

use rookery_kernel::{Kernel, Methods};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub use config::ServerConfig;
pub use leader::Role;

/// Every method of the server.
pub fn methods() -> Methods {
    let mut methods = Methods::default();
    rookery_kernel::streams::register(&mut methods);
    rookery_auth::register(&mut methods);
    methods
}

/// A server that is running.
pub struct Running {
    /// Where it listens over TCP.
    pub addr: SocketAddr,
    pub socket: Option<PathBuf>,
    role: watch::Receiver<Role>,
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

    /// Stops the server and waits for it.
    pub async fn stop(self) -> Result<(), String> {
        self.stop.cancel();
        self.done.await.map_err(|e| e.to_string())?
    }
}

/// Builds the kernel once the server leads: migrates the database and starts the facilities.
async fn lead(config: &ServerConfig) -> Result<Kernel, String> {
    let store = rookery_store::Store::open(rookery_store::StoreOptions::new(&config.database_url))
        .await
        .map_err(|e| format!("opening the database: {e}"))?;
    let mut kernel_config = rookery_kernel::Config::new(&config.public_url);
    kernel_config.auth = config.auth;
    kernel_config.oidc = config.oidc.clone();
    kernel_config.admins = config.admins.clone();
    let kernel = Kernel::new(kernel_config, store, methods());
    kernel.methods.started(&kernel);
    Ok(kernel)
}

/// Starts a server: it listens at once, and leads when it gets the lock.
pub async fn start(mut config: ServerConfig) -> Result<Running, String> {
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
    let done = tokio::spawn(run(config, listener, socket, edge, role_tx, stop.clone()));
    Ok(Running {
        addr,
        socket: socket_path,
        role,
        stop,
        done,
    })
}

async fn run(
    config: ServerConfig,
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
    let mut held_lock = None;
    let led = async {
        let lock = held_lock.insert(leader::Lock::acquire(&config.lock_url).await?);
        let kernel = tokio::select! {
            biased;
            lost = lock.lost() => return Err(lost),
            kernel = lead(&config) => kernel?,
        };
        tracing::info!("this server holds the lock and leads");
        edge.set_kernel(kernel.clone());
        let _ = role.send(Role::Leader);
        Err::<(), String>(lock.lost().await)
    };
    let result = tokio::select! {
        _ = stop.cancelled() => Ok(()),
        led = led => led,
    };
    if let Some(kernel) = edge.kernel() {
        kernel.shutdown.cancel();
    }
    quit.cancel();
    let _ = tcp.await;
    if let Some(unix) = unix {
        let _ = unix.await;
    }
    if let Some(kernel) = edge.kernel() {
        kernel.store.close().await;
    }
    // A graceful stop drains requests before another server can take over.
    drop(held_lock);
    result
}

/// `rookery serve`: runs until SIGINT or SIGTERM, or until the lock is lost.
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
