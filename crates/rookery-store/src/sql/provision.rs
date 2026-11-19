//! Creating, reconciling and revoking a plugin's role, schemas, views and grants
//! (docs/platform-split.md, "Raw SQL / The role"). Everything here runs as the kernel's database user, in one
//! transaction per plugin, under an advisory lock so two kernels on a database agree.
//!
//! The kernel never reads or runs anything in a plugin's raw schema (`x_<plugin>`) as itself: a view or
//! function there is the plugin's code, and what the kernel's user selects from it would run as the kernel's
//! user. What the kernel needs from that schema it asks through the plugin's own role.

use std::collections::BTreeSet;

use sqlx::{PgConnection, Row};

use super::grants::{self, Grant, Object};
use super::names::{Names, quote_ident, quote_literal};
use super::scram;
use super::{Sql, views};
use crate::collections::{CollectionDef, load_stored, lock_plugin};
use crate::error::{Error, Result};
use crate::seal::Key as SecretKey;
use crate::util::random_token;

/// The limits a role carries: `ALTER ROLE ... IN DATABASE ... SET`.
pub const STATEMENT_TIMEOUT: &str = "30s";
pub const IDLE_IN_TRANSACTION_TIMEOUT: &str = "15s";
pub const LOCK_TIMEOUT: &str = "10s";
/// The temporary file space one session of the role may use (`temp_file_limit`), where the kernel may set it.
pub const TEMP_FILE_LIMIT: &str = "2GB";
/// How many connections the role may hold at once, the kernel's pool and direct connections together.
pub const CONNECTION_LIMIT: i32 = 10;

/// What enabling raw SQL for a plugin made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enabled {
    pub role: String,
    pub raw_schema: String,
    pub view_schema: String,
    pub direct: bool,
}

/// The role's search path: its raw schema, its views, and the schema pgvector is installed in, so that
/// `vector` and its operators resolve without qualifying them. The role cannot create anything there.
pub fn search_path(names: &Names, vector_schema: Option<&str>) -> String {
    let mut p = format!(
        "{}, {}",
        quote_ident(&names.raw_schema),
        quote_ident(&names.view_schema)
    );
    if let Some(v) = vector_schema {
        p.push_str(", ");
        p.push_str(&quote_ident(v));
    }
    p
}

/// What the role's `ALTER ROLE ... SET` settings are, as `(name, value)` for a plugin: the search path and
/// the timeouts the specification sets, and a lock timeout.
pub fn role_settings(names: &Names, vector_schema: Option<&str>) -> Vec<(&'static str, String)> {
    vec![
        ("search_path", search_path(names, vector_schema)),
        ("statement_timeout", format!("'{STATEMENT_TIMEOUT}'")),
        (
            "idle_in_transaction_session_timeout",
            format!("'{IDLE_IN_TRANSACTION_TIMEOUT}'"),
        ),
        ("lock_timeout", format!("'{LOCK_TIMEOUT}'")),
    ]
}

/// The comment on a role that says which deployment's plugin it belongs to.
fn marker(database: &str, plugin: &str) -> String {
    format!("rookery-sql:{database}:{plugin}")
}

/// One role change at a time in this database: privileges on the database and on `public` are one catalog row
/// each, and two transactions updating it fail with `tuple concurrently updated`.
pub(super) async fn lock_catalog(tx: &mut sqlx::PgConnection) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('rookery.sql.catalog'))")
        .execute(tx)
        .await?;
    Ok(())
}

pub(crate) fn aad(plugin: &str) -> Vec<u8> {
    format!("plugin_sql:{plugin}").into_bytes()
}

/// A new random password: 43 characters of `A-Za-z0-9-_`.
fn new_password() -> String {
    random_token("")
}

impl Sql<'_> {
    /// Makes pgvector available outside the kernel's schema. The role can use no object of `crow`, and without
    /// `public` or another schema of its own pgvector's `vector` type and operators would be out of its reach.
    /// With `CREATE EXTENSION ... vector` run by the kernel's user, the extension lands in the schema named like
    /// that user (`crow`) on a default deployment, so it is moved to `public` (the extension is relocatable, and
    /// columns refer to the type by id). Best effort: a server without pgvector, or a user who may not, leaves
    /// raw SQL without it.
    async fn ensure_vector(&self) {
        let r = async {
            let mut tx = self.store.pool().begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock(hashtext('rookery.extension.vector'))")
                .execute(&mut *tx)
                .await?;
            let at: Option<String> = sqlx::query_scalar(
                "SELECT n.nspname::text FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace WHERE e.extname = 'vector'",
            )
            .fetch_optional(&mut *tx)
            .await?;
            match at.as_deref() {
                None => {
                    sqlx::query("CREATE EXTENSION IF NOT EXISTS vector SCHEMA public")
                        .execute(&mut *tx)
                        .await?;
                }
                Some("crow") => {
                    sqlx::query("ALTER EXTENSION vector SET SCHEMA public").execute(&mut *tx).await?;
                }
                Some(_) => {}
            }
            tx.commit().await
        }
        .await;
        if let Err(e) = r {
            tracing::warn!(error = %e, "pgvector is not available to raw SQL");
        }
    }

    /// The schema pgvector is installed in, if it is.
    pub async fn vector_schema(&self) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT n.nspname::text FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace WHERE e.extname = 'vector'",
        )
        .fetch_optional(self.store.pool())
        .await?)
    }

    /// Brings the plugin's role, schemas, views and grants to what its manifest asks for, and records them.
    /// Safe to repeat: every plugin load calls it.
    pub async fn enable(&self, plugin: &str, direct: bool) -> Result<Enabled> {
        let names = self.names(plugin)?;
        // Before any role exists or logs in: a plugin role must not be able to take the kernel's advisory
        // locks. When the database is not sealed and the kernel cannot seal it, the plugin's role (from an
        // earlier run) is ended as well, so that nothing of raw SQL stays reachable.
        if let Err(e) = self.seal_advisory_locks().await {
            let _ = self.disable(plugin).await;
            return Err(e);
        }
        let key = self.store.key()?;
        let mut tx = self.store.pool().begin().await?;
        lock_plugin(&mut tx, plugin).await?;
        lock_catalog(&mut tx).await?;
        let database: String = sqlx::query_scalar("SELECT current_database()::text")
            .fetch_one(&mut *tx)
            .await?;
        let row = sqlx::query(
            "SELECT direct, password_version FROM rookery.plugin_sql WHERE plugin = $1",
        )
        .bind(plugin)
        .fetch_optional(&mut *tx)
        .await?;
        // A new credential on first use and whenever `direct` changes: what services held stops working.
        let (rotate, version) = match &row {
            Some(r) if r.try_get::<bool, _>("direct")? == direct => {
                (false, r.try_get::<i32, _>("password_version")?)
            }
            Some(r) => (true, r.try_get::<i32, _>("password_version")? + 1),
            None => (true, 1),
        };
        let password = if rotate { Some(new_password()) } else { None };
        let verifier = password.as_deref().map(scram::verifier);

        let owned = marker(&database, plugin);
        let existing: Option<Option<String>> = sqlx::query_scalar(
            "SELECT shobj_description(oid, 'pg_authid') FROM pg_roles WHERE rolname = $1",
        )
        .bind(&names.role)
        .fetch_optional(&mut *tx)
        .await?;
        // A new role starts without the four powers below (the defaults of CREATE ROLE). An existing one is
        // set back only when it has one: Postgres lets a user that lacks a power change it only to say so
        // (`ALTER ROLE ... NOSUPERUSER` is refused to the kernel's `CREATEROLE` user even when it changes
        // nothing), and that user could not have given the role one.
        let mut alter_attrs =
            format!("LOGIN NOCREATEROLE NOINHERIT CONNECTION LIMIT {CONNECTION_LIMIT}");
        if existing.is_some() {
            let powers: (bool, bool, bool, bool) = sqlx::query_as(
                "SELECT rolsuper, rolcreatedb, rolreplication, rolbypassrls FROM pg_roles WHERE rolname = $1",
            )
            .bind(&names.role)
            .fetch_one(&mut *tx)
            .await?;
            for (has, no) in [
                (powers.0, "NOSUPERUSER"),
                (powers.1, "NOCREATEDB"),
                (powers.2, "NOREPLICATION"),
                (powers.3, "NOBYPASSRLS"),
            ] {
                if has {
                    alter_attrs.push(' ');
                    alter_attrs.push_str(no);
                }
            }
        }
        let attrs_create = format!(
            "LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS CONNECTION LIMIT {CONNECTION_LIMIT}"
        );
        let role = quote_ident(&names.role);
        match existing {
            None => {
                let v = match &verifier {
                    Some(v) => v.clone(),
                    // The row said a credential exists but the role is gone (dropped by hand): make a new one.
                    None => scram::verifier(&new_password()),
                };
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "CREATE ROLE {role} {attrs_create} PASSWORD {}",
                    quote_literal(&v)?
                )))
                .execute(&mut *tx)
                .await?;
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "COMMENT ON ROLE {role} IS {}",
                    quote_literal(&owned)?
                )))
                .execute(&mut *tx)
                .await?;
            }
            Some(comment) if comment.as_deref() == Some(owned.as_str()) => {
                let pw = match &verifier {
                    Some(v) => format!(" PASSWORD {}", quote_literal(v)?),
                    None => String::new(),
                };
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "ALTER ROLE {role} {alter_attrs}{pw}"
                )))
                .execute(&mut *tx)
                .await?;
            }
            Some(_) => {
                return Err(Error::conflict(format!(
                    "the Postgres role {} exists and belongs to another deployment or to something else; \
                     give this deployment a role tag (ROOKERY_DATABASE_ROLE_TAG) so its roles have names of their own",
                    names.role
                )));
            }
        }
        // The kernel's user is a member of the role, so it can hand it its schema and end its sessions.
        let version_num: String = sqlx::query_scalar("SHOW server_version_num")
            .fetch_one(&mut *tx)
            .await?;
        let grant_member = if version_num.parse::<i32>().unwrap_or(0) >= 160000 {
            format!("GRANT {role} TO CURRENT_USER WITH INHERIT TRUE, SET TRUE")
        } else {
            format!("GRANT {role} TO CURRENT_USER")
        };
        sqlx::query(sqlx::AssertSqlSafe(grant_member.to_string()))
            .execute(&mut *tx)
            .await?;

        // Settings: the role may have changed its own defaults; start from none.
        let db = quote_ident(&database);
        sqlx::query(sqlx::AssertSqlSafe(format!("ALTER ROLE {role} RESET ALL")))
            .execute(&mut *tx)
            .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER ROLE {role} IN DATABASE {db} RESET ALL"
        )))
        .execute(&mut *tx)
        .await?;
        self.ensure_vector().await;
        let vector_schema = self.vector_schema().await?;
        for (name, value) in role_settings(&names, vector_schema.as_deref()) {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER ROLE {role} IN DATABASE {db} SET {name} = {value}"
            )))
            .execute(&mut *tx)
            .await?;
        }

        // The temporary files one session may write: a sort or hash of a query that does not fit in memory goes
        // to disk, and a role's statements may run 30 s on a few connections. Only a superuser, or a user granted
        // `SET` on the parameter (`GRANT SET ON PARAMETER temp_file_limit`, Postgres 15 and later), can set it,
        // and then the role cannot change it back. Best effort: where the kernel may not, the server's own
        // `temp_file_limit` in postgresql.conf is the limit (docs/data.md, "Raw SQL / What is not prevented").
        {
            let mut sp = sqlx::Acquire::begin(&mut *tx).await?;
            let set = sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER ROLE {role} IN DATABASE {db} SET temp_file_limit = '{TEMP_FILE_LIMIT}'"
            )))
            .execute(&mut *sp)
            .await;
            match set {
                Ok(_) => sp.commit().await?,
                Err(e) => {
                    tracing::debug!(error = %e, "temp_file_limit of a plugin role not set");
                    sp.rollback().await?;
                }
            }
        }

        // Schemas.
        let raw = quote_ident(&names.raw_schema);
        let view = quote_ident(&names.view_schema);
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE SCHEMA IF NOT EXISTS {raw} AUTHORIZATION {role}"
        )))
        .execute(&mut *tx)
        .await?;
        let owner: String = sqlx::query_scalar(
            "SELECT pg_get_userbyid(nspowner)::text FROM pg_namespace WHERE nspname = $1",
        )
        .bind(&names.raw_schema)
        .fetch_one(&mut *tx)
        .await?;
        if owner != names.role {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER SCHEMA {raw} OWNER TO {role}"
            )))
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "REVOKE ALL ON SCHEMA {raw} FROM PUBLIC"
        )))
        .execute(&mut *tx)
        .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE SCHEMA IF NOT EXISTS {view}"
        )))
        .execute(&mut *tx)
        .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "REVOKE ALL ON SCHEMA {view} FROM PUBLIC"
        )))
        .execute(&mut *tx)
        .await?;
        // Nothing may create objects in `public` through PUBLIC (the default since Postgres 15).
        sqlx::query("REVOKE CREATE ON SCHEMA public FROM PUBLIC")
            .execute(&mut *tx)
            .await?;

        // Views, over the collections as stored.
        let defs: Vec<CollectionDef> = load_stored(&mut tx, plugin).await?;
        // The row first: `sync` and the collection code consult it.
        let sealed = match (&password, &row) {
            (Some(pw), _) => Some(seal(key, plugin, pw)?),
            _ => None,
        };
        match sealed {
            Some(sealed) => {
                sqlx::query(
                    "INSERT INTO rookery.plugin_sql (plugin, role_name, direct, password_nonce, password_sealed, password_version, key_id) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7) \
                     ON CONFLICT (plugin) DO UPDATE SET role_name = $2, direct = $3, password_nonce = $4, \
                     password_sealed = $5, password_version = $6, key_id = $7, direct_bypass_reported_at = NULL, updated_at = now()",
                )
                .bind(plugin)
                .bind(&names.role)
                .bind(direct)
                .bind(sealed.nonce)
                .bind(sealed.ciphertext)
                .bind(version)
                .bind(sealed.key_id)
                .execute(&mut *tx)
                .await?;
            }
            None => {
                sqlx::query(
                    "UPDATE rookery.plugin_sql SET role_name = $2, updated_at = now() WHERE plugin = $1",
                )
                .bind(plugin)
                .bind(&names.role)
                .execute(&mut *tx)
                .await?;
            }
        }
        let view_list = views::sync(&mut tx, &names, &defs).await?;

        // Grants: exactly what is due, and nothing the role holds directly beyond it.
        let current = current_grants(&mut tx, &names.role).await?;
        let due = grants::due(&names, &database, &view_list);
        let diff = grants::diff(&current, &due);
        for g in &diff.revoke {
            sqlx::query(sqlx::AssertSqlSafe(g.revoke_sql(&names.role).to_string()))
                .execute(&mut *tx)
                .await?;
        }
        for g in &diff.grant {
            sqlx::query(sqlx::AssertSqlSafe(g.grant_sql(&names.role).to_string()))
                .execute(&mut *tx)
                .await?;
        }
        // The role is a member of nothing.
        let memberships: Vec<String> = sqlx::query_scalar(
            "SELECT pg_get_userbyid(m.roleid)::text FROM pg_auth_members m WHERE m.member = (SELECT oid FROM pg_roles WHERE rolname = $1)",
        )
        .bind(&names.role)
        .fetch_all(&mut *tx)
        .await?;
        for g in memberships {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "REVOKE {} FROM {role}",
                quote_ident(&g)
            )))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        if rotate {
            // Sessions opened with the old credential end; the next statement opens a pool with the new one.
            self.forget(plugin).await;
            self.terminate_sessions(&names.role).await;
        }
        Ok(Enabled {
            role: names.role,
            raw_schema: names.raw_schema,
            view_schema: names.view_schema,
            direct,
        })
    }

    /// Takes everything the plugin's role could reach away: no login, no password, no views, no grants, no
    /// sessions. The role stays (it owns the raw schema, whose data is kept) and so does the raw schema.
    /// Returns whether the plugin had raw SQL.
    pub async fn disable(&self, plugin: &str) -> Result<bool> {
        let names = self.names(plugin)?;
        // Most plugins never had raw SQL, and this runs for each one that loads: no locks for them. (The row
        // is deleted in the transaction that revokes everything, so a plugin without one has nothing left.)
        if !self.is_enabled(plugin).await? {
            return Ok(false);
        }
        let mut tx = self.store.pool().begin().await?;
        lock_plugin(&mut tx, plugin).await?;
        lock_catalog(&mut tx).await?;
        let had = sqlx::query("DELETE FROM rookery.plugin_sql WHERE plugin = $1")
            .bind(plugin)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0;
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)")
                .bind(&names.role)
                .fetch_one(&mut *tx)
                .await?;
        if exists {
            let database: String = sqlx::query_scalar("SELECT current_database()::text")
                .fetch_one(&mut *tx)
                .await?;
            let comment: Option<String> = sqlx::query_scalar(
                "SELECT shobj_description(oid, 'pg_authid') FROM pg_roles WHERE rolname = $1",
            )
            .bind(&names.role)
            .fetch_one(&mut *tx)
            .await?;
            // Only a role this deployment made is touched.
            if comment.as_deref() == Some(marker(&database, plugin).as_str()) {
                let role = quote_ident(&names.role);
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "ALTER ROLE {role} NOLOGIN PASSWORD NULL CONNECTION LIMIT 0"
                )))
                .execute(&mut *tx)
                .await?;
                let current = current_grants(&mut tx, &names.role).await?;
                for g in &current {
                    sqlx::query(sqlx::AssertSqlSafe(g.revoke_sql(&names.role).to_string()))
                        .execute(&mut *tx)
                        .await?;
                }
            }
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA IF EXISTS {} CASCADE",
            quote_ident(&names.view_schema)
        )))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.forget(plugin).await;
        self.terminate_sessions(&names.role).await;
        Ok(had)
    }

    /// Revokes raw SQL from every plugin that has it and is not in `keep`: a plugin that was removed, or
    /// whose manifest no longer asks for it. Returns the ones it revoked.
    pub async fn sweep(&self, keep: &BTreeSet<String>) -> Result<Vec<String>> {
        let have: Vec<String> = sqlx::query_scalar("SELECT plugin FROM rookery.plugin_sql")
            .fetch_all(self.store.pool())
            .await?;
        let mut out = Vec::new();
        for p in have {
            if !keep.contains(&p) {
                self.disable(&p).await?;
                out.push(p);
            }
        }
        Ok(out)
    }

    /// Replaces the plugin's credential. What held the old one (a service's connections) stops working when
    /// it reconnects.
    pub async fn rotate(&self, plugin: &str) -> Result<()> {
        let names = self.names(plugin)?;
        let key = self.store.key()?;
        let mut tx = self.store.pool().begin().await?;
        lock_plugin(&mut tx, plugin).await?;
        lock_catalog(&mut tx).await?;
        let version: Option<i32> = sqlx::query_scalar(
            "SELECT password_version FROM rookery.plugin_sql WHERE plugin = $1 FOR UPDATE",
        )
        .bind(plugin)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(version) = version else {
            return Err(Error::not_found(format!("plugin {plugin} has no raw SQL")));
        };
        let password = new_password();
        let sealed = seal(key, plugin, &password)?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER ROLE {} PASSWORD {}",
            quote_ident(&names.role),
            quote_literal(&scram::verifier(&password))?
        )))
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE rookery.plugin_sql SET password_nonce = $2, password_sealed = $3, password_version = $4, \
             key_id = $5, direct_bypass_reported_at = NULL, updated_at = now() WHERE plugin = $1",
        )
        .bind(plugin)
        .bind(sealed.nonce)
        .bind(sealed.ciphertext)
        .bind(version + 1)
        .bind(sealed.key_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.forget(plugin).await;
        self.terminate_sessions(&names.role).await;
        Ok(())
    }

    /// The role's credential and whether services may use it: `(password, version, direct)`.
    pub(crate) async fn credential(&self, plugin: &str) -> Result<Option<(String, i32, bool)>> {
        let row = sqlx::query(
            "SELECT key_id, password_nonce, password_sealed, password_version, direct FROM rookery.plugin_sql WHERE plugin = $1",
        )
        .bind(plugin)
        .fetch_optional(self.store.pool())
        .await?;
        let Some(r) = row else { return Ok(None) };
        let sealed = crate::seal::Sealed {
            key_id: r.try_get("key_id")?,
            nonce: r.try_get("password_nonce")?,
            ciphertext: r.try_get("password_sealed")?,
        };
        let plain = self.store.key()?.open(&aad(plugin), &sealed)?;
        let password = String::from_utf8(plain)
            .map_err(|_| Error::internal("a sealed password is not text"))?;
        Ok(Some((
            password,
            r.try_get("password_version")?,
            r.try_get("direct")?,
        )))
    }

    /// The connection string a plugin's services get as `ROOKERY_DATABASE_URL`: the role's own credentials,
    /// `ROOKERY_SERVICE_DATABASE_URL`'s host and port when the deployment sets them (services may reach Postgres
    /// at another address than the kernel does), and the deployment's database. `None` unless the plugin
    /// has `database.direct`.
    pub async fn connection_url(&self, plugin: &str) -> Result<Option<String>> {
        let Some((password, _, direct)) = self.credential(plugin).await? else {
            return Ok(None);
        };
        if !direct {
            return Ok(None);
        }
        let names = self.names(plugin)?;
        let own = self.store.pool().connect_options();
        let (mut host, mut port) = (own.get_host().to_string(), own.get_port());
        if let Some(url) = &self.store.data.sql.service_url {
            let o: sqlx::postgres::PgConnectOptions = url.parse()?;
            host = o.get_host().to_string();
            port = o.get_port();
        }
        let mut url = url::Url::parse("postgres://placeholder/")
            .map_err(|e| Error::internal(e.to_string()))?;
        url.set_host(Some(&host))
            .map_err(|e| Error::internal(e.to_string()))?;
        let _ = url.set_port(Some(port));
        let _ = url.set_username(&names.role);
        let _ = url.set_password(Some(&password));
        url.set_path(own.get_database().unwrap_or("crow"));
        Ok(Some(url.to_string()))
    }

    /// Ends the role's sessions (the kernel's user is a member of the role).
    pub(crate) async fn terminate_sessions(&self, role: &str) {
        let _ = sqlx::query(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename = $1 AND pid <> pg_backend_pid()",
        )
        .bind(role)
        .execute(self.store.pool())
        .await;
    }
}

fn seal(key: &SecretKey, plugin: &str, password: &str) -> Result<crate::seal::Sealed> {
    key.seal(&aad(plugin), password.as_bytes())
}

/// The privileges `role` holds itself (not through `PUBLIC`) on objects it does not own.
pub async fn current_grants(conn: &mut PgConnection, role: &str) -> Result<BTreeSet<Grant>> {
    let rows = sqlx::query(
        "WITH r AS (SELECT oid FROM pg_roles WHERE rolname = $1) \
         SELECT 'database' AS kind, d.datname::text AS obj, NULL::text AS col, a.privilege_type::text AS priv \
           FROM pg_database d, aclexplode(d.datacl) a, r WHERE a.grantee = r.oid \
         UNION ALL SELECT 'schema', n.nspname::text, NULL, a.privilege_type::text \
           FROM pg_namespace n, aclexplode(n.nspacl) a, r WHERE a.grantee = r.oid AND n.nspowner <> r.oid \
         UNION ALL SELECT CASE WHEN c.relkind = 'S' THEN 'sequence' ELSE 'table' END, \
           chr(34) || replace(n.nspname::text, chr(34), chr(34) || chr(34)) || chr(34) || '.' || chr(34) || replace(c.relname::text, chr(34), chr(34) || chr(34)) || chr(34), NULL, a.privilege_type::text \
           FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace, aclexplode(c.relacl) a, r \
           WHERE a.grantee = r.oid AND c.relowner <> r.oid \
         UNION ALL SELECT 'column', chr(34) || replace(n.nspname::text, chr(34), chr(34) || chr(34)) || chr(34) || '.' || chr(34) || replace(c.relname::text, chr(34), chr(34) || chr(34)) || chr(34), at.attname::text, a.privilege_type::text \
           FROM pg_attribute at JOIN pg_class c ON c.oid = at.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace, \
           aclexplode(at.attacl) a, r WHERE a.grantee = r.oid AND c.relowner <> r.oid \
         UNION ALL SELECT 'function', p.oid::regprocedure::text, NULL, a.privilege_type::text \
           FROM pg_proc p, aclexplode(p.proacl) a, r WHERE a.grantee = r.oid AND p.proowner <> r.oid \
         UNION ALL SELECT 'language', l.lanname::text, NULL, a.privilege_type::text \
           FROM pg_language l, aclexplode(l.lanacl) a, r WHERE a.grantee = r.oid \
         UNION ALL SELECT 'type', format_type(t.oid, NULL), NULL, a.privilege_type::text \
           FROM pg_type t, aclexplode(t.typacl) a, r WHERE a.grantee = r.oid AND t.typowner <> r.oid \
         UNION ALL SELECT 'tablespace', s.spcname::text, NULL, a.privilege_type::text \
           FROM pg_tablespace s, aclexplode(s.spcacl) a, r WHERE a.grantee = r.oid \
         UNION ALL SELECT 'fdw', w.fdwname::text, NULL, a.privilege_type::text \
           FROM pg_foreign_data_wrapper w, aclexplode(w.fdwacl) a, r WHERE a.grantee = r.oid \
         UNION ALL SELECT 'server', s.srvname::text, NULL, a.privilege_type::text \
           FROM pg_foreign_server s, aclexplode(s.srvacl) a, r WHERE a.grantee = r.oid",
    )
    .bind(role)
    .fetch_all(conn)
    .await?;
    let mut out = BTreeSet::new();
    for r in rows {
        let kind: String = r.try_get("kind")?;
        let obj: String = r.try_get("obj")?;
        let col: Option<String> = r.try_get("col")?;
        let priv_: String = r.try_get("priv")?;
        let on = match kind.as_str() {
            "database" => Object::Database(obj),
            "schema" => Object::Schema(obj),
            "table" => Object::Table(obj),
            "sequence" => Object::Sequence(obj),
            "column" => Object::Column {
                table: obj,
                column: col.unwrap_or_default(),
            },
            "function" => Object::Function(obj),
            "language" => Object::Language(obj),
            "type" => Object::Type(obj),
            "tablespace" => Object::Tablespace(obj),
            "fdw" => Object::ForeignDataWrapper(obj),
            "server" => Object::ForeignServer(obj),
            _ => continue,
        };
        out.insert(Grant {
            on,
            privilege: priv_,
        });
    }
    Ok(out)
}
