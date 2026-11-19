//! The kernel's advisory locks belong to the kernel (docs/data.md, "Raw SQL / Before first use").
//!
//! The kernel serialises work with Postgres advisory locks: every collection write takes the change log's
//! ordering lock, plugin loads, topics, blobs, logs, leases, jobs and the catalog have theirs, and the leader
//! election holds one for the life of the process. Advisory locks are one namespace per database, every role
//! may call the functions that take them, and `pg_locks` shows the keys in use to everyone: a plugin's SQL
//! that held one would stall the kernel for as long as it liked (30 s through `rookery.sql`, with no limit on a
//! direct connection). Secret keys would not help.
//!
//! So a plugin's role must not be able to execute any `pg_advisory_*` or `pg_try_advisory_*` function.
//! `EXECUTE` on the built-in functions is granted to `PUBLIC`; it can be revoked per database, and only by a
//! superuser (the functions belong to the bootstrap superuser, and revoking a privilege one neither owns nor
//! was granted with grant option does nothing but print a warning, even for the owner of the database). The
//! kernel therefore:
//!
//! - checks, at start and before it enables raw SQL for a plugin, that neither `PUBLIC` nor any plugin role
//!   can execute any of them ([`Sql::open_advisory_functions`]);
//! - when one can, tries the revoke itself (works when the kernel's database user is a superuser, as in a
//!   development setup), and grants the functions to its own user so that it keeps them;
//! - when that is not permitted, raw SQL is unavailable ([`Sql::seal_advisory_locks`] fails with
//!   `UNAVAILABLE`, naming the step), and `enable` ends the plugin's role instead of leaving it able to log in.
//!
//! A deployment that never uses raw SQL is not touched: nothing here runs for it.

use super::Sql;
use super::names::quote_ident;
use super::provision::lock_catalog;
use crate::error::{Error, Result};

/// The advisory lock functions, found in the catalog rather than listed: a new Postgres version may add more.
const ADVISORY: &str =
    "p.pronamespace = 'pg_catalog'::regnamespace AND p.proname LIKE '%advisory%'";

/// Revokes `EXECUTE` on every advisory lock function from `PUBLIC` and grants it to the current user. The same
/// statement is in `deploy/postgres/init.sh` and `docs/data.md`.
pub const SEAL_SQL: &str = r#"DO $seal$
DECLARE f record;
BEGIN
  FOR f IN SELECT format('%I.%I(%s)', n.nspname, p.proname, pg_get_function_identity_arguments(p.oid)) AS sig
           FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
           WHERE n.nspname = 'pg_catalog' AND p.proname LIKE '%advisory%'
  LOOP
    EXECUTE format('REVOKE EXECUTE ON FUNCTION %s FROM PUBLIC', f.sig);
    EXECUTE format('GRANT EXECUTE ON FUNCTION %s TO %I', f.sig, current_user);
  END LOOP;
END
$seal$"#;

/// [`SEAL_SQL`] granting the functions to `kernel_role` instead of the current user: what an operator runs as a
/// superuser (the deployment's compose file names its kernel role `crow`).
pub fn seal_statement(kernel_role: &str) -> String {
    SEAL_SQL.replace(
        "f.sig, current_user",
        &format!("f.sig, '{}'", kernel_role.replace('\'', "''")),
    )
}

impl Sql<'_> {
    /// The advisory lock functions that `PUBLIC` or a plugin's role can execute, as `name(args)`. Empty when
    /// the database is sealed.
    pub async fn open_advisory_functions(&self) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT p.proname::text || '(' || pg_get_function_identity_arguments(p.oid) || ')' FROM pg_proc p \
             WHERE {ADVISORY} AND (has_function_privilege('public', p.oid, 'EXECUTE') \
               OR EXISTS (SELECT 1 FROM rookery.plugin_sql s JOIN pg_roles r ON r.rolname = s.role_name \
                          WHERE has_function_privilege(r.oid, p.oid, 'EXECUTE'))) \
             ORDER BY 1"
        )))
        .fetch_all(self.store.pool())
        .await?)
    }

    /// The advisory lock functions the kernel's own user cannot execute (it would lose its own locks).
    async fn kernel_lacks_advisory_functions(&self) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT p.proname::text || '(' || pg_get_function_identity_arguments(p.oid) || ')' FROM pg_proc p \
             WHERE {ADVISORY} AND NOT has_function_privilege(current_user, p.oid, 'EXECUTE') ORDER BY 1"
        )))
        .fetch_all(self.store.pool())
        .await?)
    }

    /// Makes sure no plugin role can take the kernel's advisory locks: checks, tries the revoke itself when
    /// something is open, checks again. `UNAVAILABLE` when it is still open: raw SQL must stay off.
    pub async fn seal_advisory_locks(&self) -> Result<()> {
        if self.sealed().await? {
            return Ok(());
        }
        let tried: Result<()> = async {
            let mut tx = self.store.pool().begin().await?;
            lock_catalog(&mut tx).await?;
            sqlx::query(sqlx::AssertSqlSafe(SEAL_SQL.to_string()))
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(())
        }
        .await;
        if let Err(e) = &tried {
            tracing::debug!(error = %e, "revoking advisory lock functions from PUBLIC failed");
        }
        if self.sealed().await? {
            tracing::info!(
                "advisory lock functions revoked from PUBLIC: plugin roles cannot take the kernel's locks"
            );
            return Ok(());
        }
        let db: String = sqlx::query_scalar("SELECT current_database()::text")
            .fetch_one(self.store.pool())
            .await?;
        let open = self.open_advisory_functions().await?;
        let lacks = self.kernel_lacks_advisory_functions().await?;
        let what = if lacks.is_empty() {
            format!(
                "plugin roles can still execute {} advisory lock function(s) such as {}",
                open.len(),
                open.first()
                    .map(String::as_str)
                    .unwrap_or("pg_advisory_lock(bigint)")
            )
        } else {
            format!(
                "the kernel's own database user cannot execute {} of them such as {}",
                lacks.len(),
                lacks
                    .first()
                    .map(String::as_str)
                    .unwrap_or("pg_advisory_lock(bigint)")
            )
        };
        Err(Error::Unavailable(format!(
            "raw SQL is unavailable until a database superuser seals the advisory locks of database {db}: {what}. \
             Run once, connected to {} as a superuser, the statement in docs/data.md (\"Raw SQL / Before first use\"): \
             REVOKE EXECUTE ON the pg_catalog pg_advisory_* and pg_try_advisory_* functions FROM PUBLIC and GRANT it to the \
             kernel's user. Deployments that do not use raw SQL are not affected",
            quote_ident(&db)
        )))
    }

    /// Whether the database is sealed: no plugin role can execute an advisory lock function, and the kernel's
    /// own user can.
    async fn sealed(&self) -> Result<bool> {
        Ok(self.open_advisory_functions().await?.is_empty()
            && self.kernel_lacks_advisory_functions().await?.is_empty())
    }
}
