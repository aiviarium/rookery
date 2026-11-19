//! The server's tables on Postgres.
//!
//! Every table is in the schema `rookery`, made by the numbered migrations in `migrations/`, which
//! [`Store::open`] runs. A store is cheap to clone: the clones share one pool.

pub mod audit;
pub mod auth;
pub mod error;
pub mod links;
mod migrate;
pub mod state;
#[cfg(feature = "testing")]
pub mod testing;
pub mod users;
pub mod util;

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
}

impl StoreOptions {
    pub fn new(database_url: impl Into<String>) -> Self {
        StoreOptions {
            database_url: database_url.into(),
            max_connections: 16,
        }
    }
}

#[derive(Clone)]
pub struct Store {
    pool: PgPool,
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
        Ok(Store { pool })
    }

    /// A store that connects when first used, for tests that never reach the database.
    pub fn lazy(options: StoreOptions) -> Result<Store> {
        let connect: PgConnectOptions = options.database_url.parse()?;
        let pool = PgPoolOptions::new()
            .max_connections(options.max_connections)
            .connect_lazy_with(connect);
        Ok(Store { pool })
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
}

/// The versions and names of the migrations this build has, in order.
pub fn migrations() -> Vec<(i32, &'static str)> {
    migrate::MIGRATIONS
        .iter()
        .map(|m| (m.version, m.name))
        .collect()
}
