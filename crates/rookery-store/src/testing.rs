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
    blobs: std::path::PathBuf,
    /// The role tag of the database's raw SQL roles.
    pub tag: String,
}

/// Taken while a role is made for a database that does not exist yet, which the sweep of orphan roles
/// would otherwise take for one left behind.
const ROLE_LOCK: i64 = 0x726f_6f6b_726f_6c65;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

impl TestDb {
    /// A fresh, migrated database, or `None` when there is no Postgres to test on.
    pub async fn new() -> Option<TestDb> {
        TestDb::new_with(|_| {}).await
    }

    /// Raw SQL's time limits for a test: these four, and the watchdog's margins a tenth of the defaults.
    pub fn limits(
        statement: std::time::Duration,
        tx_total: std::time::Duration,
        tx_idle: std::time::Duration,
        idle_in_transaction: std::time::Duration,
    ) -> crate::sql::exec::Limits {
        use std::time::Duration;
        crate::sql::exec::Limits {
            statement,
            tx_total,
            tx_idle,
            idle_in_transaction,
            slack: Duration::from_millis(200),
            terminate_after: Duration::from_millis(500),
            registration_grace: Duration::from_millis(200),
            ..Default::default()
        }
    }

    /// A fresh database whose raw SQL has these time limits.
    pub async fn new_with_limits(limits: crate::sql::exec::Limits) -> Option<TestDb> {
        TestDb::new_with(|o| o.sql_limits = Some(limits)).await
    }

    /// A fresh database whose store is opened with options of the test's.
    pub async fn new_with(tune: impl FnOnce(&mut StoreOptions)) -> Option<TestDb> {
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
        let tag = role_tag(&name);
        let mut options = StoreOptions::new(&url);
        options.max_connections = 8;
        options.secret_key = Some(crate::seal::Key::generate());
        let blobs = std::env::temp_dir().join(&name);
        options.blobs = Some(blobs.clone());
        options.sql_role_tag = Some(tag.clone());
        tune(&mut options);
        let store = Store::open(options)
            .await
            .expect("opening the test database");
        Some(TestDb {
            store,
            url,
            name,
            server,
            blobs,
            tag,
        })
    }

    /// A fresh database owned by an application role, as a deployment's: no superuser, with
    /// `CREATEROLE` when asked. The store migrates it as that role. [`TestDb::admin_connection`] is the
    /// server's superuser on the same database.
    pub async fn new_owned_by_app_role(create_role: bool) -> Option<TestDb> {
        let server = server_url()?;
        let mut admin = PgConnection::connect(&server)
            .await
            .expect("the Postgres of ROOKERY_TEST_POSTGRES");
        sweep(&mut admin).await;
        let random = crate::util::random_token("");
        let name = format!("{PREFIX}{}_{}", now(), &random[..12]);
        let tag = role_tag(&name);
        let app = format!("rookery_app_{}", &random[..11]);
        let password = &random[12..44];
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(ROLE_LOCK)
            .execute(&mut admin)
            .await
            .expect("locking the making of roles");
        for statement in [
            format!(
                "CREATE ROLE {app} LOGIN NOSUPERUSER NOCREATEDB {} PASSWORD '{password}'",
                if create_role {
                    "CREATEROLE"
                } else {
                    "NOCREATEROLE"
                }
            ),
            format!("COMMENT ON ROLE {app} IS 'rookery-sql:{name}:app'"),
            format!("CREATE DATABASE {name} OWNER {app}"),
        ] {
            sqlx::raw_sql(AssertSqlSafe(statement))
                .execute(&mut admin)
                .await
                .expect("making the application role and its database");
        }
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(ROLE_LOCK)
            .execute(&mut admin)
            .await
            .expect("unlocking");
        admin.close().await.ok();
        let base = url_for(&server, &name);
        let mut parsed = url::Url::parse(&base).expect("the URL of the test Postgres");
        let _ = parsed.set_username(&app);
        let _ = parsed.set_password(Some(password));
        let url = parsed.to_string();
        let mut options = StoreOptions::new(&url);
        options.max_connections = 12;
        options.secret_key = Some(crate::seal::Key::generate());
        options.sql_role_tag = Some(tag.clone());
        let blobs = std::env::temp_dir().join(&name);
        let store = Store::open(options)
            .await
            .expect("migrating a database as a role that is no superuser");
        Some(TestDb {
            store,
            url,
            name,
            server,
            blobs,
            tag,
        })
    }

    /// A connection to this database as the test server's superuser.
    pub async fn admin_connection(&self) -> PgConnection {
        PgConnection::connect(&url_for(&self.server, &self.name))
            .await
            .expect("connecting as the superuser")
    }

    /// The database's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The role tag of this database: raw SQL roles are the server's, not a database's, so each test
    /// database names its own apart (`rookery_<tag>_x_<plugin>`).
    pub fn role_tag(&self) -> &str {
        &self.tag
    }

    /// A person signed in through a test issuer, with their personal space.
    pub async fn user(&self, name: &str) -> uuid::Uuid {
        let (u, _) = self
            .store
            .users()
            .upsert_login(&crate::users::Login {
                issuer: "test".into(),
                subject: name.into(),
                name: name.into(),
                email: Some(format!("{name}@example.test")),
                ..Default::default()
            })
            .await
            .expect("signing in a test user");
        u.id
    }

    /// The options of another store on this database.
    pub fn options(&self) -> StoreOptions {
        let mut options = StoreOptions::new(&self.url);
        options.max_connections = 4;
        options.sql_role_tag = Some(self.tag.clone());
        options
    }

    /// Another store on this database, as another server process has, with this store's key.
    pub async fn second_store(&self) -> crate::Result<Store> {
        let mut options = self.options();
        options.secret_key = self.store.key().ok().cloned();
        Store::open(options).await
    }

    /// The caller of a test user.
    pub fn caller(&self, user: uuid::Uuid) -> crate::Caller {
        crate::Caller::user(user)
    }

    /// Closes the pool and drops the database. A connection a test still holds does not keep it: the
    /// database is dropped with its sessions after a few seconds.
    pub async fn close(self) {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), self.store.close()).await;
        let _ = std::fs::remove_dir_all(&self.blobs);
        if let Ok(mut admin) = PgConnection::connect(&self.server).await {
            let _ = sqlx::raw_sql(AssertSqlSafe(format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                self.name
            )))
            .execute(&mut admin)
            .await;
            drop_roles(&mut admin, &self.name).await;
            admin.close().await.ok();
        }
    }
}

/// The role tag of a test database: `t` and the random part of its name.
fn role_tag(db: &str) -> String {
    let random = db.rsplit('_').next().unwrap_or("");
    format!("t{}", &random[..random.len().min(11)])
}

/// Drops the roles of a database: they are the server's, so dropping the database leaves them. A role
/// of raw SQL names its database in its comment.
async fn drop_roles(admin: &mut PgConnection, database: &str) {
    let roles: Vec<String> = sqlx::query_scalar(
        "SELECT rolname::text FROM pg_roles WHERE shobj_description(oid, 'pg_authid') LIKE $1",
    )
    .bind(format!("rookery-sql:{database}:%"))
    .fetch_all(&mut *admin)
    .await
    .unwrap_or_default();
    for r in roles {
        let _ = sqlx::query(AssertSqlSafe(format!("DROP ROLE IF EXISTS \"{r}\"")))
            .execute(&mut *admin)
            .await;
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

/// Drops test databases that outlived their tests, and the roles of test databases that are gone.
async fn sweep(admin: &mut PgConnection) {
    let _ = sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(ROLE_LOCK)
        .execute(&mut *admin)
        .await;
    let orphans: Vec<String> = sqlx::query_scalar(
        "SELECT rolname::text FROM pg_roles r WHERE shobj_description(oid, 'pg_authid') LIKE 'rookery-sql:rookery_test_%'
         AND NOT EXISTS (SELECT 1 FROM pg_database d WHERE d.datname = split_part(shobj_description(r.oid, 'pg_authid'), ':', 2))",
    )
    .fetch_all(&mut *admin)
    .await
    .unwrap_or_default();
    for r in orphans {
        let _ = sqlx::query(AssertSqlSafe(format!("DROP ROLE IF EXISTS \"{r}\"")))
            .execute(&mut *admin)
            .await;
    }
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(ROLE_LOCK)
        .execute(&mut *admin)
        .await;
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
