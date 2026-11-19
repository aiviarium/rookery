//! A plugin's SQL migrations: `migrations/sql/0001_init.sql`, `0002_add_scores.sql`, ... run in order, each
//! exactly once, in its own transaction, as the plugin's role (docs/platform-split.md, "Raw SQL / SQL
//! migrations").
//!
//! - What ran is recorded in `rookery.plugin_sql_migrations` with each file's checksum. A file that was applied
//!   and has changed stops the plugin from loading.
//! - The role cannot write `crow`, so the record cannot be in the migration's own transaction. A marker row in
//!   the plugin's raw schema (`x_<plugin>._rookery_migrations`) is: the file's statements and the marker commit
//!   together. After a crash between that commit and the kernel's record, the next load reads the marker (as
//!   the role: the kernel never runs anything of the plugin's as itself) and records the migration then. A
//!   plugin that edits its own marker table can make the kernel skip or repeat its own migrations and
//!   nothing else.
//! - Two kernels on one database take an advisory lock for the plugin, so a file runs once.

use sqlx::{Connection, Executor, Row};

use super::Sql;
use super::exec::{MIGRATION_APP, map_error, role_options};
use super::names::quote_ident;
use crate::error::{Error, Result};

/// One file of `migrations/sql/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlMigration {
    pub version: i64,
    /// The file's name, `0001_init.sql`.
    pub file: String,
    pub sql: String,
}

impl SqlMigration {
    /// The checksum recorded for the file: a SHA-256 of its text, in hex.
    pub fn checksum(&self) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(self.sql.as_bytes()))
    }
}

/// The version a file name starts with: `0001_init.sql` is 1. The name is digits, `_`, then letters, digits,
/// `_` and `-`, ending in `.sql`; the version is at least 1.
pub fn parse_name(file: &str) -> Result<i64> {
    let bad = || {
        Error::invalid(format!(
            "migrations/sql/{file}: name SQL migrations <number>_<name>.sql (0001_init.sql)"
        ))
    };
    let stem = file.strip_suffix(".sql").ok_or_else(bad)?;
    let (digits, name) = stem.split_once('_').ok_or_else(bad)?;
    if digits.is_empty()
        || digits.len() > 9
        || !digits.bytes().all(|b| b.is_ascii_digit())
        || name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(bad());
    }
    let v: i64 = digits.parse().map_err(|_| bad())?;
    if v < 1 {
        return Err(Error::invalid(format!(
            "migrations/sql/{file}: migration numbers start at 1"
        )));
    }
    Ok(v)
}

/// The files `(name, text)` in the order they run. Two files with one number are refused.
pub fn order(files: Vec<(String, String)>) -> Result<Vec<SqlMigration>> {
    let mut out = Vec::new();
    for (file, sql) in files {
        let version = parse_name(&file)?;
        out.push(SqlMigration { version, file, sql });
    }
    out.sort_by(|a, b| a.version.cmp(&b.version).then_with(|| a.file.cmp(&b.file)));
    if let Some(w) = out.windows(2).find(|w| w[0].version == w[1].version) {
        return Err(Error::invalid(format!(
            "migrations/sql/{} and {} have the same number",
            w[0].file, w[1].file
        )));
    }
    Ok(out)
}

/// The files of `dir/migrations/sql`, in order. A missing folder is no files.
pub fn read_dir(dir: &std::path::Path) -> Result<Vec<SqlMigration>> {
    let folder = dir.join("migrations").join("sql");
    let Ok(entries) = std::fs::read_dir(&folder) else {
        return Ok(Vec::new());
    };
    let mut files = Vec::new();
    for e in entries.filter_map(|e| e.ok()) {
        let path = e.path();
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if !path.is_file() || !name.ends_with(".sql") {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|err| Error::invalid(format!("migrations/sql/{name}: {err}")))?;
        files.push((name, text));
    }
    order(files)
}

/// The pending ones: those not applied. An applied file whose checksum differs is an error naming it.
pub fn pending<'a>(
    files: &'a [SqlMigration],
    applied: &[(i64, String)],
) -> Result<Vec<&'a SqlMigration>> {
    let mut out = Vec::new();
    for f in files {
        match applied.iter().find(|(v, _)| *v == f.version) {
            Some((_, checksum)) if *checksum != f.checksum() => {
                return Err(Error::invalid(format!(
                    "migrations/sql/{} was changed after it was applied; an applied migration is never edited, add a new one",
                    f.file
                )));
            }
            Some(_) => {}
            None => out.push(f),
        }
    }
    Ok(out)
}

const MARKER: &str = "_rookery_migrations";
/// Statement time a migration gets: longer than the role's 30 s, for index builds and backfills.
const MIGRATION_TIMEOUT: &str = "10min";
/// [`MIGRATION_TIMEOUT`] in seconds: what the watchdog allows a migration's statement.
pub(crate) const MIGRATION_TIMEOUT_SECS: u64 = 600;

impl Sql<'_> {
    /// Applies the plugin's pending SQL migrations. A failing file leaves its changes rolled back and fails with
    /// an error that names it.
    pub async fn migrate(&self, plugin: &str, files: &[SqlMigration]) -> Result<Vec<i64>> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let names = self.names(plugin)?;
        let (password, _, _) = self.credential(plugin).await?.ok_or_else(|| {
            Error::not_found(format!("plugin {plugin} has no raw SQL (database.sql)"))
        })?;
        let vector_schema = self.vector_schema().await?;
        // Held for the whole run, on a connection of its own: a second kernel waits here.
        let mut lock = self.store.pool().acquire().await?;
        sqlx::query("SELECT pg_advisory_lock(hashtext('rookery.sqlmigrations:' || $1))")
            .bind(plugin)
            .execute(&mut *lock)
            .await?;
        let result = async {
            let mut conn = sqlx::PgConnection::connect_with(&role_options(
                (*self.store.pool().connect_options()).clone(),
                &names,
                &password,
                MIGRATION_APP,
                &self.limits(),
                vector_schema.as_deref(),
            ))
            .await
            .map_err(map_error)?;
            // Written down as the kernel's own, before the file's SQL runs: the watchdog gives such a session
            // its longer statement time, and no other (whatever its `application_name`).
            if let Err(e) = super::exec::register_session(
                self.store.pool(),
                &mut conn,
                &names.role,
                "migration",
            )
            .await
            {
                let _ = conn.close().await;
                return Err(map_error(e));
            }
            let r = self
                .run_files(plugin, &names.raw_schema, files, &mut conn)
                .await;
            let _ = conn.close().await;
            r
        }
        .await;
        let _ = sqlx::query("SELECT pg_advisory_unlock(hashtext('rookery.sqlmigrations:' || $1))")
            .bind(plugin)
            .execute(&mut *lock)
            .await;
        result
    }

    async fn run_files(
        &self,
        plugin: &str,
        raw_schema: &str,
        files: &[SqlMigration],
        conn: &mut sqlx::PgConnection,
    ) -> Result<Vec<i64>> {
        let marker = format!("{}.{}", quote_ident(raw_schema), quote_ident(MARKER));
        conn.execute(sqlx::AssertSqlSafe(format!(
            "CREATE TABLE IF NOT EXISTS {marker} (version bigint PRIMARY KEY, file text NOT NULL, \
                 checksum text NOT NULL, applied_at timestamptz NOT NULL DEFAULT now())"
        )))
        .await
        .map_err(|e| {
            Error::invalid(format!(
                "preparing the raw schema for migrations: {}",
                map_error(e)
            ))
        })?;
        // Migrations that committed but were not yet recorded (a crash between the two).
        let marked = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT version, file, checksum FROM {marker}"
        )))
        .fetch_all(&mut *conn)
        .await
        .map_err(map_error)?;
        for r in &marked {
            let (version, file, checksum): (i64, String, String) = (
                r.try_get("version").map_err(map_error)?,
                r.try_get("file").map_err(map_error)?,
                r.try_get("checksum").map_err(map_error)?,
            );
            sqlx::query(
                "INSERT INTO rookery.plugin_sql_migrations (plugin, version, name, checksum) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (plugin, version) DO NOTHING",
            )
            .bind(plugin)
            .bind(version)
            .bind(file)
            .bind(checksum)
            .execute(self.store.pool())
            .await?;
        }
        let applied: Vec<(i64, String)> = sqlx::query_as(
            "SELECT version, checksum FROM rookery.plugin_sql_migrations WHERE plugin = $1",
        )
        .bind(plugin)
        .fetch_all(self.store.pool())
        .await?;
        let todo = pending(files, &applied)?;
        let mut ran = Vec::new();
        for m in todo {
            let fail = |e: Error| Error::invalid(format!("migrations/sql/{}: {e}", m.file));
            conn.execute("BEGIN")
                .await
                .map_err(|e| fail(map_error(e)))?;
            let step = async {
                conn.execute(sqlx::AssertSqlSafe(format!(
                    "SET LOCAL statement_timeout = '{MIGRATION_TIMEOUT}'"
                )))
                .await?;
                // The simple query protocol: a file is several statements, and it has no parameters.
                conn.execute(sqlx::AssertSqlSafe(m.sql.clone())).await?;
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "INSERT INTO {marker} (version, file, checksum) VALUES ($1, $2, $3)"
                )))
                .bind(m.version)
                .bind(&m.file)
                .bind(m.checksum())
                .execute(&mut *conn)
                .await?;
                conn.execute("COMMIT").await
            }
            .await;
            if let Err(e) = step {
                let _ = conn.execute("ROLLBACK").await;
                return Err(fail(map_error(e)));
            }
            sqlx::query(
                "INSERT INTO rookery.plugin_sql_migrations (plugin, version, name, checksum) VALUES ($1, $2, $3, $4)",
            )
            .bind(plugin)
            .bind(m.version)
            .bind(&m.file)
            .bind(m.checksum())
            .execute(self.store.pool())
            .await?;
            ran.push(m.version);
        }
        Ok(ran)
    }
}
