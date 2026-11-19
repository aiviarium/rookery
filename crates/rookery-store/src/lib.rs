//! The server's tables on Postgres.
//!
//! Every table is in the schema `rookery`, made by the numbered migrations in `migrations/`, which
//! [`Store::open`] runs. A store is cheap to clone: the clones share one pool.

pub mod access;
pub mod activity;
pub mod actor;
pub mod audit;
pub mod auth;
pub mod blobs;
pub mod caller;
pub mod collections;
pub mod connections;
pub mod documents;
pub mod entities;
pub mod error;
pub mod feed;
pub mod history;
pub mod idempotency;
pub mod jobs;
pub mod kv;
pub mod layout;
pub mod leader;
pub mod leases;
pub mod links;
mod migrate;
pub mod notify;
pub mod pins;
pub mod recents;
pub mod refs;
pub mod seal;
pub mod search;
pub mod secrets;
pub mod settings;
pub mod spaces;
pub mod sql;
pub mod state;
pub mod terminals;
#[cfg(feature = "testing")]
pub mod testing;
pub mod topics;
pub mod uri;
pub mod users;
pub mod util;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

pub use caller::{Caller, EVERYONE, Level};
pub use error::{Error, Result, SchemaDifference};
pub use uri::Uri;

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
    /// `ROOKERY_DATABASE_ROLE_TAG`: puts this deployment's raw SQL roles in a namespace of their own, as
    /// Postgres roles are shared by every database of a server.
    pub sql_role_tag: Option<String>,
    /// `ROOKERY_SERVICE_DATABASE_URL`: the host and port plugins' services reach Postgres at, when they are
    /// not the server's.
    pub service_database_url: Option<String>,
    /// The time limits of raw SQL; the defaults unless a test shortens them.
    pub sql_limits: Option<sql::exec::Limits>,
    /// Typed tables of other crates, such as those of hosts, that are collections of Rookery's too.
    pub system_tables: Vec<collections::system::SystemTable>,
    /// How many changes the change feed keeps in memory for each subscriber before one that lags reads
    /// them from the change log instead.
    pub feed_capacity: usize,
}

impl StoreOptions {
    pub fn new(database_url: impl Into<String>) -> Self {
        StoreOptions {
            database_url: database_url.into(),
            max_connections: 16,
            secret_key: None,
            blobs: None,
            feed_capacity: 4096,
            system_tables: Vec::new(),
            sql_role_tag: None,
            service_database_url: None,
            sql_limits: None,
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
    /// What the clones share of the data layer: the collections and the change feed.
    data: Arc<DataState>,
}

/// The collections in memory and the change feed, shared by every clone of a store.
pub(crate) struct DataState {
    pub registry: std::sync::RwLock<
        std::collections::HashMap<(String, String), Arc<collections::Collection>>,
    >,
    /// Held while the registry is read from the database and replaced, so that a reload which read the
    /// table before another plugin's apply committed cannot write its older view over a newer one.
    pub reload_lock: tokio::sync::Mutex<()>,
    pub feed: feed::FeedState,
    /// The channel the triggers of typed tables tell the change feed on, once Rookery's collections are made.
    pub channel: std::sync::RwLock<Option<String>>,
    /// Raw SQL: the plugins' pools, their open transactions and the deployment's settings for it.
    pub sql: sql::State,
    /// Rookery's typed tables registered as collections, which a reload keeps.
    pub system: std::sync::RwLock<
        Vec<(
            Arc<collections::system::SystemTable>,
            collections::CollectionDef,
        )>,
    >,
    /// Log collections: per table, the end of the period whose partition and the next one exist.
    pub log_partitions:
        std::sync::Mutex<std::collections::HashMap<String, chrono::DateTime<chrono::Utc>>>,
}

impl DataState {
    fn new(options: &StoreOptions) -> Arc<DataState> {
        let feed_capacity = options.feed_capacity;
        Arc::new(DataState {
            registry: Default::default(),
            reload_lock: tokio::sync::Mutex::new(()),
            feed: feed::FeedState::new(feed_capacity),
            system: Default::default(),
            channel: Default::default(),
            sql: sql::State::new(
                options.sql_role_tag.clone(),
                options.service_database_url.clone(),
                options.sql_limits.unwrap_or_default(),
            ),
            log_partitions: Default::default(),
        })
    }
}

impl Store {
    /// Connects and runs the migrations that have not run yet.
    pub async fn open(options: StoreOptions) -> Result<Store> {
        let tables = options.system_tables.clone();
        let store = Store::connect(options).await?;
        migrate::run(&store.pool).await?;
        store.reload().await?;
        store.collections().install_system(&tables).await?;
        // People from before the data layer, or brought in by an import, get their entity and space.
        store.users().backfill().await?;
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
        Self::from_parts(pool, StoreOptions::new(""))
    }

    fn from_parts(pool: PgPool, options: StoreOptions) -> Store {
        let data = DataState::new(&options);
        Store {
            pool,
            key: options.secret_key.map(Arc::new),
            blobs: options.blobs.map(Arc::new),
            fence: None,
            data,
        }
    }

    /// Reads the collections and the change feed's head from the database again.
    pub async fn reload(&self) -> Result<()> {
        self.collections().reload().await?;
        self.feed().init_head().await
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
        kv::Kv { store: self }
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
        layout::Layout { store: self }
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
    pub fn entities(&self) -> entities::Entities<'_> {
        entities::Entities { store: self }
    }
    pub fn refs(&self) -> refs::Refs<'_> {
        refs::Refs { store: self }
    }
    /// The definitions of collections and their tables.
    pub fn collections(&self) -> collections::Collections<'_> {
        collections::Collections { store: self }
    }
    /// The collections of one plugin, for reads and writes.
    pub fn data<'a>(&'a self, plugin: &'a str) -> collections::Data<'a> {
        collections::Data {
            store: self,
            plugin,
        }
    }
    pub fn feed(&self) -> feed::Feed<'_> {
        feed::Feed { store: self }
    }
    pub fn history(&self) -> history::History<'_> {
        history::History { store: self }
    }
    pub fn search(&self) -> search::Search<'_> {
        search::Search { store: self }
    }
    /// Raw SQL: plugins' roles, views and raw schemas, and running their statements.
    pub fn sql(&self) -> sql::Sql<'_> {
        sql::Sql { store: self }
    }
    pub fn spaces(&self) -> spaces::Spaces<'_> {
        spaces::Spaces { store: self }
    }
    pub fn documents(&self) -> documents::Documents<'_> {
        documents::Documents { store: self }
    }
    pub fn recents(&self) -> recents::Recents<'_> {
        recents::Recents { store: self }
    }
    pub fn pins(&self) -> pins::Pins<'_> {
        pins::Pins { store: self }
    }
    pub fn activity(&self) -> activity::ActivityFeed<'_> {
        activity::ActivityFeed { store: self }
    }
}

/// The versions and names of the migrations this build has, in order.
pub fn migrations() -> Vec<(i32, &'static str)> {
    migrate::MIGRATIONS
        .iter()
        .map(|m| (m.version, m.name))
        .collect()
}
