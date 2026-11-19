//! A fresh database per test, on the Postgres of `ROOKERY_TEST_POSTGRES`.
//!
//! Each database is copied from a template that is migrated once per set of migrations, so a test pays
//! for a `CREATE DATABASE … TEMPLATE`, not for the migrations. Test programs share one server and may
//! run side by side: the template is made under an advisory lock. A database is dropped when its test
//! ends, and one that outlived its test, such as after a crash, is dropped by a later run.
//!
//! Without `ROOKERY_TEST_POSTGRES` the tests that need a database are skipped, except in CI, where they
//! fail.

use sqlx::AssertSqlSafe;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::{Connection, PgConnection};

use crate::{Store, StoreOptions};

const PREFIX: &str = "rookery_test_";
const TEMPLATE_PREFIX: &str = "rookery_tpl_";
/// Taken while a template is made.
const TEMPLATE_LOCK: i64 = 0x726f_6f6b_7465_7374;
/// Seconds after which a test database is taken as left behind.
const STALE: u64 = 1800;

/// The Postgres the tests use, or `None` to skip.
pub fn server_url() -> Option<String> {
    match std::env::var("ROOKERY_TEST_POSTGRES") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ if std::env::var_os("CI").is_some() => {
            panic!("CI sets ROOKERY_TEST_POSTGRES for the tests of the store")
        }
        _ => None,
    }
}

/// The same server, another database.
pub fn url_for(base: &str, database: &str) -> String {
    let (head, query) = match base.split_once('?') {
        Some((head, query)) => (head, format!("?{query}")),
        None => (base, String::new()),
    };
    let start = head.find("://").map_or(0, |at| at + 3);
    let server = match head[start..].find('/') {
        Some(slash) => &head[..start + slash],
        None => head,
    };
    format!("{server}/{database}{query}")
}

pub struct TestDb {
    pub store: Store,
    pub url: String,
    name: String,
    server: String,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

impl TestDb {
    /// A fresh, migrated database, or `None` when there is no Postgres to test on.
    pub async fn new() -> Option<TestDb> {
        let server = server_url()?;
        let mut admin = PgConnection::connect(&server)
            .await
            .expect("the Postgres of ROOKERY_TEST_POSTGRES");
        sweep(&mut admin).await;
        let template = template(&server, &mut admin).await;
        let random = crate::util::random_token("");
        let name = format!("{PREFIX}{}_{}", now(), &random[..12]);
        // Copying fails while something is connected to the template, such as a test program making it.
        let mut tries = 0;
        loop {
            match sqlx::raw_sql(AssertSqlSafe(format!(
                "CREATE DATABASE {name} TEMPLATE {template}"
            )))
            .execute(&mut admin)
            .await
            {
                Ok(_) => break,
                Err(e) if tries < 50 && e.to_string().contains("being accessed by other users") => {
                    tries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                Err(e) => panic!("creating {name}: {e}"),
            }
        }
        admin.close().await.ok();
        let url = url_for(&server, &name);
        let mut options = StoreOptions::new(&url);
        options.max_connections = 8;
        let store = Store::open(options)
            .await
            .expect("opening the test database");
        Some(TestDb {
            store,
            url,
            name,
            server,
        })
    }

    /// Closes the pool and drops the database.
    pub async fn close(self) {
        self.store.close().await;
        if let Ok(mut admin) = PgConnection::connect(&self.server).await {
            let _ = sqlx::raw_sql(AssertSqlSafe(format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                self.name
            )))
            .execute(&mut admin)
            .await;
            admin.close().await.ok();
        }
    }
}

/// The template for this build's migrations, made the first time.
async fn template(server: &str, admin: &mut PgConnection) -> String {
    let name = format!("{TEMPLATE_PREFIX}{}", crate::migrate::fingerprint());
    if exists(admin, &name).await {
        return name;
    }
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(TEMPLATE_LOCK)
        .execute(&mut *admin)
        .await
        .expect("locking");
    if !exists(admin, &name).await {
        sqlx::raw_sql(AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {name} WITH (FORCE)"
        )))
        .execute(&mut *admin)
        .await
        .ok();
        sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&mut *admin)
            .await
            .expect("creating the template");
        let store = Store::open(StoreOptions::new(url_for(server, &name)))
            .await
            .expect("migrating the template");
        store.close().await;
        sqlx::raw_sql(AssertSqlSafe(format!(
            "ALTER DATABASE {name} WITH IS_TEMPLATE true"
        )))
        .execute(&mut *admin)
        .await
        .expect("marking the template");
    }
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(TEMPLATE_LOCK)
        .execute(&mut *admin)
        .await
        .expect("unlocking");
    name
}

/// Drops test databases that outlived their tests.
async fn sweep(admin: &mut PgConnection) {
    let names: Vec<String> =
        sqlx::query_scalar("SELECT datname FROM pg_database WHERE datname LIKE $1")
            .bind(format!("{PREFIX}%"))
            .fetch_all(&mut *admin)
            .await
            .unwrap_or_default();
    let cutoff = now().saturating_sub(STALE);
    for name in names {
        let born = name
            .trim_start_matches(PREFIX)
            .split('_')
            .next()
            .and_then(|t| t.parse::<u64>().ok());
        if born.is_some_and(|born| born < cutoff) {
            let _ = sqlx::raw_sql(AssertSqlSafe(format!(
                "DROP DATABASE IF EXISTS {name} WITH (FORCE)"
            )))
            .execute(&mut *admin)
            .await;
        }
    }
}

/// `let db = test_db!();` in a test: a fresh database, or the test returns early when there is none.
#[macro_export]
macro_rules! test_db {
    () => {
        match $crate::testing::TestDb::new().await {
            Some(db) => db,
            None => {
                eprintln!("skipped: ROOKERY_TEST_POSTGRES is not set");
                return;
            }
        }
    };
}

async fn exists(admin: &mut PgConnection, name: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1 AND datistemplate)",
    )
    .bind(name)
    .fetch_one(admin)
    .await
    .expect("reading the databases")
}
