//! The server's tables on Postgres.
//!
//! Every table is in the schema `rookery`, made by the numbered migrations in `migrations/`, which
//! [`Store::open`] runs. A store is cheap to clone: the clones share one pool.

pub mod access;
pub mod audit;
pub mod auth;
pub mod blobs;
pub mod connections;
pub mod error;
pub mod jobs;
pub mod kv;
pub mod layout;
pub mod leader;
pub mod leases;
pub mod links;
mod migrate;
pub mod notify;
pub mod seal;
pub mod secrets;
pub mod settings;
pub mod state;
pub mod terminals;
#[cfg(feature = "testing")]
pub mod testing;
pub mod topics;
pub mod users;
pub mod util;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

pub use error::{Error, Result};

/// How to open a [`Store`].
#[derive(Debug, Clone)]
pub struct StoreOptions {
    /// `ROOKERY_DATABASE_URL`.
    pub database_url: String,
    pub max_connections: u32,
    /// The sealing key. Without one the server runs, and refuses what needs it: secrets and the
    /// credentials of connections.
    pub secret_key: Option<seal::Key>,
    /// `ROOKERY_BLOBS`: the folder of the blobs' files.
    pub blobs: Option<PathBuf>,
}

impl StoreOptions {
    pub fn new(database_url: impl Into<String>) -> Self {
        StoreOptions {
            database_url: database_url.into(),
            max_connections: 16,
            secret_key: None,
            blobs: None,
        }
    }
}

#[derive(Clone)]
pub struct Store {
    pool: PgPool,
    key: Option<Arc<seal::Key>>,
    blobs: Option<Arc<PathBuf>>,
    /// The leader's epoch its writes to jobs and leases name, or `None` for a store that leads nothing.
    fence: Option<i64>,
}

impl Store {
    /// Connects and runs the migrations that have not run yet.
    pub async fn open(options: StoreOptions) -> Result<Store> {
        let store = Store::connect(options).await?;
        migrate::run(&store.pool).await?;
        Ok(store)
    }

    /// Connects without migrating.
    pub async fn connect(options: StoreOptions) -> Result<Store> {
        let connect: PgConnectOptions = options.database_url.parse()?;
        let connect = connect
            .options([("timezone", "UTC")])
            .application_name("rookery");
        let pool = PgPoolOptions::new()
            .max_connections(options.max_connections)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(connect)
            .await?;
        Ok(Store::from_parts(pool, options))
    }

    /// A store that connects when first used, for tests that never reach the database.
    pub fn lazy(options: StoreOptions) -> Result<Store> {
        let connect: PgConnectOptions = options.database_url.parse()?;
        let pool = PgPoolOptions::new()
            .max_connections(options.max_connections)
            .connect_lazy_with(connect);
        Ok(Store::from_parts(pool, options))
    }

    /// A store on a pool of the caller's, with no key and no blobs.
    pub fn from_pool(pool: PgPool) -> Store {
        Store {
            pool,
            key: None,
            blobs: None,
            fence: None,
        }
    }

    fn from_parts(pool: PgPool, options: StoreOptions) -> Store {
        Store {
            pool,
            key: options.secret_key.map(Arc::new),
            blobs: options.blobs.map(Arc::new),
            fence: None,
        }
    }

    /// The same store, whose writes to jobs and leases fail with `UNAVAILABLE` once the leader's epoch
    /// is no longer `epoch`.
    pub fn fenced(&self, epoch: i64) -> Store {
        Store {
            fence: Some(epoch),
            ..self.clone()
        }
    }

    /// The epoch this store's writes name.
    pub fn fence(&self) -> Option<i64> {
        self.fence
    }

    /// The same store with another sealing key, or none.
    pub fn with_key(&self, key: Option<seal::Key>) -> Store {
        Store {
            key: key.map(Arc::new),
            ..self.clone()
        }
    }

    /// The sealing key, or `UNAVAILABLE` when the server has none.
    pub fn key(&self) -> Result<&seal::Key> {
        self.key.as_deref().ok_or_else(|| {
            Error::Unavailable(
                "this server has no secret key: set ROOKERY_SECRET_KEY or ROOKERY_SECRET_KEY_FILE"
                    .into(),
            )
        })
    }

    /// The folder of the blobs, or `UNAVAILABLE` when `ROOKERY_BLOBS` is not set.
    pub fn blob_dir(&self) -> Result<&Path> {
        self.blobs.as_deref().map(PathBuf::as_path).ok_or_else(|| {
            Error::Unavailable("this server keeps no blobs: set ROOKERY_BLOBS".into())
        })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn close(&self) {
        self.pool.close().await
    }

    pub fn users(&self) -> users::Users<'_> {
        users::Users { pool: &self.pool }
    }
    pub fn auth(&self) -> auth::Auth<'_> {
        auth::Auth { pool: &self.pool }
    }
    pub fn links(&self) -> links::Links<'_> {
        links::Links { pool: &self.pool }
    }
    pub fn state(&self) -> state::State<'_> {
        state::State { pool: &self.pool }
    }
    pub fn audit(&self) -> audit::Audit<'_> {
        audit::Audit { pool: &self.pool }
    }
    pub fn access(&self) -> access::Access<'_> {
        access::Access { pool: &self.pool }
    }
    pub fn settings(&self) -> settings::Settings<'_> {
        settings::Settings { pool: &self.pool }
    }
    pub fn secrets(&self) -> secrets::Secrets<'_> {
        secrets::Secrets { store: self }
    }
    pub fn kv(&self) -> kv::Kv<'_> {
        kv::Kv { pool: &self.pool }
    }
    pub fn connections(&self) -> connections::Connections<'_> {
        connections::Connections { store: self }
    }
    pub fn blobs(&self) -> blobs::Blobs<'_> {
        blobs::Blobs { store: self }
    }
    pub fn terminals(&self) -> terminals::Terminals<'_> {
        terminals::Terminals {
            pool: &self.pool,
            fence: self.fence,
        }
    }
    pub fn layout(&self) -> layout::Layout<'_> {
        layout::Layout { pool: &self.pool }
    }
    pub fn jobs(&self) -> jobs::Jobs<'_> {
        jobs::Jobs {
            pool: &self.pool,
            fence: self.fence,
        }
    }
    pub fn leases(&self) -> leases::Leases<'_> {
        leases::Leases {
            pool: &self.pool,
            fence: self.fence,
        }
    }
    pub fn topics(&self) -> topics::Topics<'_> {
        topics::Topics { pool: &self.pool }
    }
    pub fn notify(&self) -> notify::Notify<'_> {
        notify::Notify { pool: &self.pool }
    }
}

/// The versions and names of the migrations this build has, in order.
pub fn migrations() -> Vec<(i32, &'static str)> {
    migrate::MIGRATIONS
        .iter()
        .map(|m| (m.version, m.name))
        .collect()
}
