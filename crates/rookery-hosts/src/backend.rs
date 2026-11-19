//! rookery-core's store on the server's tables. Each kind of state has its place: profiles in
//! `rookery.hosts`, SSH access in `host_access`, the key ring in `host_keys`, provisioners, resources,
//! secrets and host credentials in `rookery.secrets`, registries as connections of the type `registry`,
//! settings as the global settings of the plugin `rookery`, key-value entries in `rookery.kv`, and the
//! messages of the hosts' channels in `host_messages` and `host_cursors`. Only the installation's own
//! scope exists. Secrets, credentials and keys are sealed with the server's key, as the facilities seal
//! theirs; the rest is plain.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rookery_api::{From as MessageFrom, Keep, Level as MessageLevel, Message};
use rookery_core::store::{
    self, Backend, Change, Entry, Error, Filter, Job, JobFilter, Result, Rows, StateRow, Stored,
    Table, When, kind,
};
use rookery_store::seal::{self, Key, Sealed};
use serde_json::Value;
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

use crate::db::Db;

/// The plugin whose settings, secrets and key-value entries rookery-core's are.
pub const PLUGIN: &str = "rookery";
/// The secrets that belong to a thing of rookery's, such as a registry's login, by `ID/NAME`.
const OWNED: &str = "rookery.owned";
/// What logs in to a host, by the host's id.
const CREDENTIALS: &str = "rookery.credentials";

pub struct ServerBackend {
    db: Db,
    key: Option<Arc<Key>>,
    /// The leader's epoch that taking in a host's channel names, which only the leader does.
    fence: Option<i64>,
}

impl ServerBackend {
    pub fn new(db: Db, key: Option<Key>, fence: Option<i64>) -> ServerBackend {
        ServerBackend {
            db,
            key: key.map(Arc::new),
            fence,
        }
    }

    /// Runs `work` in one transaction.
    fn tx<T, F>(&self, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(
                &'c mut PgConnection,
                Option<Arc<Key>>,
            ) -> futures_util::future::BoxFuture<'c, Result<T>>
            + Send
            + 'static,
    {
        let key = self.key.clone();
        self.db.run(move |pool: PgPool| {
            Box::pin(async move {
                let mut tx = pool.begin().await.map_err(db)?;
                let done = work(&mut tx, key).await?;
                tx.commit().await.map_err(db)?;
                Ok(done)
            })
        })
    }
}

pub(crate) fn db(e: sqlx::Error) -> Error {
    if let sqlx::Error::Database(d) = &e
        && d.is_unique_violation()
    {
        return Error::Taken {
            kind: String::new(),
            label: String::new(),
        };
    }
    Error::Database(e.to_string())
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ── Where each kind is ──────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum Value_ {
    /// A JSON column; the value is its text.
    Json(&'static str),
    /// A JSON column holding the value as a JSON string.
    JsonText(&'static str),
    Bytes(&'static str),
    /// `key_id`, `nonce` and `ciphertext`, with the row named in the associated data by the key.
    Sealed,
}

/// A kind kept in rows of a table of its own, keyed by one column.
#[derive(Clone, Copy)]
struct Place {
    table: &'static str,
    key: &'static str,
    /// The key is a uuid column given as 32 hex digits.
    uuid: bool,
    label: Option<&'static str>,
    value: Value_,
    /// Columns that pick the kind's rows out of the table, and that a new row gets.
    filter: &'static [(&'static str, &'static str)],
    /// Columns a new row gets as well.
    insert: &'static [(&'static str, &'static str)],
}

enum Spot {
    Plain(Place),
    /// A host's device on the tailnet, a column of its row.
    Tailnet,
    /// A resource's record or specification, a column of its row.
    Resource(&'static str),
    /// Secrets kept by a name of rookery-core's with an id of their own: `plugin`.
    Named(&'static str),
    /// A secret with a name of its own, and those that belong to a thing.
    Secret,
    /// What no server keeps.
    None,
}

const HOSTS: Place = Place {
    table: "hosts",
    key: "id",
    uuid: false,
    label: Some("name"),
    value: Value_::Json("profile"),
    filter: &[],
    insert: &[],
};
const ACCESS: Place = Place {
    table: "host_access",
    key: "host",
    uuid: false,
    label: None,
    value: Value_::Json("value"),
    filter: &[],
    insert: &[],
};
const KEYS: Place = Place {
    table: "host_keys",
    key: "id",
    uuid: false,
    label: Some("name"),
    value: Value_::Sealed,
    filter: &[],
    insert: &[],
};
const ENTRIES: Place = Place {
    table: "provisioners",
    key: "id",
    uuid: false,
    label: Some("name"),
    value: Value_::Json("value"),
    filter: &[("kind", "entry")],
    insert: &[],
};
const ARGUMENTS: Place = Place {
    table: "provisioners",
    key: "id",
    uuid: false,
    label: None,
    value: Value_::Json("value"),
    filter: &[("kind", "arguments")],
    insert: &[],
};
const PACKAGES: Place = Place {
    table: "package_state",
    key: "name",
    uuid: false,
    label: None,
    value: Value_::Bytes("value"),
    filter: &[],
    insert: &[],
};
const REGISTRIES: Place = Place {
    table: "connections",
    key: "id",
    uuid: true,
    label: Some("name"),
    value: Value_::Json("config"),
    filter: &[("type", "registry"), ("scope", "global")],
    insert: &[("created_by", "rookery")],
};
const SETTINGS: Place = Place {
    table: "settings",
    key: "key",
    uuid: false,
    label: None,
    value: Value_::JsonText("value"),
    filter: &[("plugin", PLUGIN), ("scope", "global")],
    insert: &[("updated_by", "rookery")],
};
const SECRETS: Place = Place {
    table: "secrets",
    key: "id",
    uuid: true,
    label: Some("name"),
    value: Value_::Sealed,
    filter: &[("plugin", PLUGIN), ("scope", "global")],
    insert: &[],
};

/// The kinds a server keeps, in the order a listing of everything gives them.
const KINDS: &[&str] = &[
    kind::PROFILE,
    kind::TAILNET,
    kind::ACCESS,
    kind::CREDENTIALS,
    kind::KEY,
    kind::PROVISIONER_ENTRY,
    kind::PROVISIONER,
    kind::RESOURCE,
    kind::SPEC,
    kind::SETTING,
    kind::REGISTRY,
    kind::SECRET,
    kind::PLUGIN,
];

fn spot(kind: &str) -> Spot {
    match kind {
        kind::PROFILE => Spot::Plain(HOSTS),
        kind::TAILNET => Spot::Tailnet,
        kind::ACCESS => Spot::Plain(ACCESS),
        kind::CREDENTIALS => Spot::Named(CREDENTIALS),
        kind::KEY => Spot::Plain(KEYS),
        kind::PROVISIONER_ENTRY => Spot::Plain(ENTRIES),
        kind::PROVISIONER => Spot::Plain(ARGUMENTS),
        kind::RESOURCE => Spot::Resource("record"),
        kind::SPEC => Spot::Resource("spec"),
        kind::SETTING => Spot::Plain(SETTINGS),
        kind::REGISTRY => Spot::Plain(REGISTRIES),
        kind::SECRET => Spot::Secret,
        kind::PLUGIN => Spot::Plain(PACKAGES),
        _ => Spot::None,
    }
}

fn not_kept(kind: &str) -> Error {
    Error::Given(format!("rookery serve on Postgres keeps no {kind}"))
}

fn own(scope: &str) -> Result<()> {
    match scope == store::OWN {
        true => Ok(()),
        false => Err(Error::Given(
            "rookery serve on Postgres keeps the installation's own state alone".into(),
        )),
    }
}

/// The 32 hex digits of a uuid, as rookery-core names things.
fn bare(id: Uuid) -> String {
    id.simple().to_string()
}

fn uuid_of(name: &str) -> Result<Uuid> {
    Uuid::parse_str(name).map_err(|_| Error::Given(format!("{name:?} is not an id")))
}

fn sealed_key(key: &Option<Arc<Key>>) -> Result<&Key> {
    key.as_deref().ok_or_else(|| {
        Error::Sealing(
            "this server has no secret key: set ROOKERY_SECRET_KEY or ROOKERY_SECRET_KEY_FILE to keep keys and secrets"
                .into(),
        )
    })
}

fn open(key: &Option<Arc<Key>>, aad: &[u8], sealed: Sealed) -> Result<Vec<u8>> {
    sealed_key(key)?
        .open(aad, &sealed)
        .map_err(|e| Error::Sealing(e.to_string()))
}

fn close(key: &Option<Arc<Key>>, aad: &[u8], plain: &[u8]) -> Result<Sealed> {
    sealed_key(key)?
        .seal(aad, plain)
        .map_err(|e| Error::Sealing(e.to_string()))
}

fn json_of(kind: &str, name: &str, bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice(bytes)
        .map_err(|e| Error::Given(format!("{kind} {name:?} is not JSON: {e}")))
}

impl Place {
    fn key_in(&self, n: usize) -> String {
        match self.uuid {
            true => format!("${n}::uuid"),
            false => format!("${n}"),
        }
    }

    fn key_out(&self) -> String {
        match self.uuid {
            true => format!("replace({}::text, '-', '')", self.key),
            false => self.key.to_owned(),
        }
    }

    fn filter_sql(&self) -> String {
        self.filter
            .iter()
            .map(|(col, value)| format!(" AND {col} = '{value}'"))
            .collect()
    }

    fn select(&self) -> String {
        let label = self.label.unwrap_or("NULL::text");
        let value = match self.value {
            Value_::Json(col) => {
                format!(
                    "{col}::text AS vt, NULL::bytea AS vb, NULL::text AS kid, NULL::bytea AS nonce, NULL::bytea AS ct"
                )
            }
            Value_::JsonText(col) => format!(
                "({col} #>> '{{}}') AS vt, NULL::bytea AS vb, NULL::text AS kid, NULL::bytea AS nonce, NULL::bytea AS ct"
            ),
            Value_::Bytes(col) => format!(
                "NULL::text AS vt, {col} AS vb, NULL::text AS kid, NULL::bytea AS nonce, NULL::bytea AS ct"
            ),
            Value_::Sealed => {
                "NULL::text AS vt, NULL::bytea AS vb, key_id AS kid, nonce, ciphertext AS ct".into()
            }
        };
        format!(
            "SELECT {} AS k, {label} AS l, {value}, updated_at AS u FROM rookery.{} WHERE true{}",
            self.key_out(),
            self.table,
            self.filter_sql()
        )
    }

    fn row(
        &self,
        kind: &str,
        key: &Option<Arc<Key>>,
        r: &sqlx::postgres::PgRow,
    ) -> Result<StateRow> {
        let name: String = r.get("k");
        let value = match self.value {
            Value_::Json(_) | Value_::JsonText(_) => r
                .get::<Option<String>, _>("vt")
                .unwrap_or_default()
                .into_bytes(),
            Value_::Bytes(_) => r.get::<Option<Vec<u8>>, _>("vb").unwrap_or_default(),
            Value_::Sealed => {
                let sealed = Sealed {
                    key_id: r.get::<Option<String>, _>("kid").unwrap_or_default(),
                    nonce: r.get::<Option<Vec<u8>>, _>("nonce").unwrap_or_default(),
                    ciphertext: r.get::<Option<Vec<u8>>, _>("ct").unwrap_or_default(),
                };
                let aad = match self.uuid {
                    true => seal::row(self.table, uuid_of(&name)?),
                    false => seal::named_row(self.table, &name),
                };
                open(key, &aad, sealed)?
            }
        };
        Ok(StateRow {
            kind: kind.to_owned(),
            scope: store::OWN.to_owned(),
            name,
            value,
            sealed: false,
            updated: rfc3339(r.get("u")),
            label: r.get("l"),
        })
    }

    async fn get(
        &self,
        c: &mut PgConnection,
        kind: &str,
        key: &Option<Arc<Key>>,
        name: &str,
    ) -> Result<Option<StateRow>> {
        if self.uuid && Uuid::parse_str(name).is_err() {
            return Ok(None);
        }
        let sql = format!("{} AND {} = {}", self.select(), self.key, self.key_in(1));
        let r = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(name)
            .fetch_optional(&mut *c)
            .await
            .map_err(db)?;
        r.map(|r| self.row(kind, key, &r)).transpose()
    }

    async fn labelled(
        &self,
        c: &mut PgConnection,
        kind: &str,
        key: &Option<Arc<Key>>,
        label: &str,
    ) -> Result<Option<StateRow>> {
        let Some(col) = self.label else {
            return Ok(None);
        };
        let sql = format!("{} AND {col} = $1", self.select());
        let r = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(label)
            .fetch_optional(&mut *c)
            .await
            .map_err(db)?;
        r.map(|r| self.row(kind, key, &r)).transpose()
    }

    async fn all(
        &self,
        c: &mut PgConnection,
        kind: &str,
        key: &Option<Arc<Key>>,
    ) -> Result<Vec<StateRow>> {
        let sql = format!("{} ORDER BY k", self.select());
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .fetch_all(&mut *c)
            .await
            .map_err(db)?;
        rows.iter().map(|r| self.row(kind, key, r)).collect()
    }

    async fn put(
        &self,
        c: &mut PgConnection,
        key: &Option<Arc<Key>>,
        row: &StateRow,
    ) -> Result<()> {
        if self.uuid {
            uuid_of(&row.name)?;
        }
        let mut cols: Vec<String> = vec![self.key.to_owned()];
        let mut vals: Vec<String> = vec![self.key_in(1)];
        let mut sets: Vec<String> = vec!["updated_at = now()".into()];
        let mut binds: Vec<ValueBind> = vec![];
        let mut next = 2;
        let mut param = |binds: &mut Vec<ValueBind>, bind: ValueBind| {
            binds.push(bind);
            let n = next;
            next += 1;
            format!("${n}")
        };
        for (col, value) in self.filter.iter().chain(self.insert) {
            cols.push((*col).into());
            vals.push(format!("'{value}'"));
        }
        // The label, kept as it is by a write that gives none.
        if let Some(col) = self.label {
            cols.push(col.into());
            let p = param(&mut binds, ValueBind::Opt(row.label.clone()));
            vals.push(match self.uuid {
                // A connection needs a name: a write without one names it by its id.
                true => format!("coalesce({p}, $1)"),
                false => p,
            });
            sets.push(format!(
                "{col} = coalesce({}, rookery.{}.{col})",
                vals.last().unwrap().replace(", $1)", ", NULL)"),
                self.table
            ));
        }
        match self.value {
            Value_::Json(col) => {
                json_of(&row.kind, &row.name, &row.value)?;
                let p = param(
                    &mut binds,
                    ValueBind::Text(String::from_utf8_lossy(&row.value).into_owned()),
                );
                cols.push(col.into());
                vals.push(format!("{p}::jsonb"));
                sets.push(format!("{col} = EXCLUDED.{col}"));
            }
            Value_::JsonText(col) => {
                let p = param(
                    &mut binds,
                    ValueBind::Text(String::from_utf8_lossy(&row.value).into_owned()),
                );
                cols.push(col.into());
                vals.push(format!("to_jsonb({p}::text)"));
                sets.push(format!("{col} = EXCLUDED.{col}"));
            }
            Value_::Bytes(col) => {
                let p = param(&mut binds, ValueBind::Bytes(row.value.clone()));
                cols.push(col.into());
                vals.push(p);
                sets.push(format!("{col} = EXCLUDED.{col}"));
            }
            Value_::Sealed => {
                let aad = match self.uuid {
                    true => seal::row(self.table, uuid_of(&row.name)?),
                    false => seal::named_row(self.table, &row.name),
                };
                let sealed = close(key, &aad, &row.value)?;
                for (col, bind) in [
                    ("key_id", ValueBind::Text(sealed.key_id)),
                    ("nonce", ValueBind::Bytes(sealed.nonce)),
                    ("ciphertext", ValueBind::Bytes(sealed.ciphertext)),
                ] {
                    let p = param(&mut binds, bind);
                    cols.push(col.into());
                    vals.push(p);
                    sets.push(format!("{col} = EXCLUDED.{col}"));
                }
            }
        }
        let conflict = match self.table {
            // Settings are unique by plugin, key, scope and user.
            "settings" => "(plugin, key, scope, user_id)".to_owned(),
            _ => format!("({})", self.key),
        };
        let sql = format!(
            "INSERT INTO rookery.{} ({}) VALUES ({}) ON CONFLICT {conflict} DO UPDATE SET {}",
            self.table,
            cols.join(", "),
            vals.join(", "),
            sets.join(", ")
        );
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(&row.name);
        for v in binds {
            q = match v {
                ValueBind::Text(t) => q.bind(t),
                ValueBind::Opt(t) => q.bind(t),
                ValueBind::Bytes(b) => q.bind(b),
            };
        }
        q.execute(&mut *c).await.map_err(db)?;
        Ok(())
    }

    async fn remove(&self, c: &mut PgConnection, name: &str) -> Result<bool> {
        if self.uuid && Uuid::parse_str(name).is_err() {
            return Ok(false);
        }
        let sql = format!(
            "DELETE FROM rookery.{} WHERE {} = {}{}",
            self.table,
            self.key,
            self.key_in(1),
            self.filter_sql()
        );
        let done = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(name)
            .execute(&mut *c)
            .await
            .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    async fn clear(&self, c: &mut PgConnection) -> Result<()> {
        let sql = format!(
            "DELETE FROM rookery.{} WHERE true{}",
            self.table,
            self.filter_sql()
        );
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&mut *c)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn relabel(&self, c: &mut PgConnection, name: &str, label: &str) -> Result<bool> {
        let Some(col) = self.label else {
            return Ok(false);
        };
        if self.uuid && Uuid::parse_str(name).is_err() {
            return Ok(false);
        }
        let sql = format!(
            "UPDATE rookery.{} SET {col} = $2, updated_at = now() WHERE {} = {}{}",
            self.table,
            self.key,
            self.key_in(1),
            self.filter_sql()
        );
        let done = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(name)
            .bind(label)
            .execute(&mut *c)
            .await
            .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    /// Removes the rows without a label that belong to `owner`.
    async fn forget_owned(&self, c: &mut PgConnection, owner: &str) -> Result<()> {
        if self.uuid {
            return Ok(());
        }
        let label = match self.label {
            Some(col) => format!(" AND {col} IS NULL"),
            None => String::new(),
        };
        let sql = format!(
            "DELETE FROM rookery.{} WHERE ({k} = $1 OR starts_with({k}, $1 || '/')){label}{}",
            self.table,
            self.filter_sql(),
            k = self.key
        );
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(owner)
            .execute(&mut *c)
            .await
            .map_err(db)?;
        Ok(())
    }
}

enum ValueBind {
    Text(String),
    Opt(Option<String>),
    Bytes(Vec<u8>),
}

// ── The kinds kept in a column, and the secrets ─────────────────────────────

/// A JSON column of a row of its own, `rookery.hosts.tailnet` or `rookery.resources.record`.
async fn column_get(
    c: &mut PgConnection,
    table: &str,
    col: &str,
    name: Option<&str>,
) -> Result<Vec<(String, String, DateTime<Utc>)>> {
    let sql = format!(
        "SELECT id, {col}::text, updated_at FROM rookery.{table} WHERE {col} IS NOT NULL AND ($1::text IS NULL OR id = $1) ORDER BY id"
    );
    let rows: Vec<(String, String, DateTime<Utc>)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(name)
        .fetch_all(&mut *c)
        .await
        .map_err(db)?;
    Ok(rows)
}

/// A secret's row: its id, name, the key it is sealed under, nonce, ciphertext and when it changed.
type SealedRow = (Uuid, String, String, Vec<u8>, Vec<u8>, DateTime<Utc>);

/// Secrets kept by a name of rookery-core's, `plugin` telling which: the rows of a kind such as the
/// credentials of hosts, or the secrets that belong to a thing.
async fn named_get(
    c: &mut PgConnection,
    key: &Option<Arc<Key>>,
    plugin: &str,
    name: Option<&str>,
) -> Result<Vec<(String, Vec<u8>, DateTime<Utc>)>> {
    let rows: Vec<SealedRow> = sqlx::query_as(
        "SELECT id, name, key_id, nonce, ciphertext, updated_at FROM rookery.secrets
         WHERE plugin = $1 AND scope = 'global' AND ($2::text IS NULL OR name = $2) ORDER BY name",
    )
    .bind(plugin)
    .bind(name)
    .fetch_all(&mut *c)
    .await
    .map_err(db)?;
    rows.into_iter()
        .map(|(id, name, key_id, nonce, ciphertext, at)| {
            let plain = open(
                key,
                &seal::row("secrets", id),
                Sealed {
                    key_id,
                    nonce,
                    ciphertext,
                },
            )?;
            Ok((name, plain, at))
        })
        .collect()
}

async fn named_put(
    c: &mut PgConnection,
    key: &Option<Arc<Key>>,
    plugin: &str,
    name: &str,
    value: &[u8],
) -> Result<()> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM rookery.secrets WHERE plugin = $1 AND scope = 'global' AND name = $2",
    )
    .bind(plugin)
    .bind(name)
    .fetch_optional(&mut *c)
    .await
    .map_err(db)?;
    let id = id.unwrap_or_else(Uuid::now_v7);
    let sealed = close(key, &seal::row("secrets", id), value)?;
    sqlx::query(
        "INSERT INTO rookery.secrets (id, plugin, name, scope, key_id, nonce, ciphertext) VALUES ($1, $2, $3, 'global', $4, $5, $6)
         ON CONFLICT (id) DO UPDATE SET key_id = EXCLUDED.key_id, nonce = EXCLUDED.nonce, ciphertext = EXCLUDED.ciphertext, updated_at = now()",
    )
    .bind(id)
    .bind(plugin)
    .bind(name)
    .bind(&sealed.key_id)
    .bind(&sealed.nonce)
    .bind(&sealed.ciphertext)
    .execute(&mut *c)
    .await
    .map_err(db)?;
    Ok(())
}

async fn named_remove(c: &mut PgConnection, plugin: &str, name: &str) -> Result<bool> {
    let done = sqlx::query(
        "DELETE FROM rookery.secrets WHERE plugin = $1 AND scope = 'global' AND name = $2",
    )
    .bind(plugin)
    .bind(name)
    .execute(&mut *c)
    .await
    .map_err(db)?;
    Ok(done.rows_affected() > 0)
}

async fn named_forget_owned(c: &mut PgConnection, plugin: &str, owner: &str) -> Result<()> {
    sqlx::query(
        "DELETE FROM rookery.secrets WHERE plugin = $1 AND scope = 'global' AND (name = $2 OR starts_with(name, $2 || '/'))",
    )
    .bind(plugin)
    .bind(owner)
    .execute(&mut *c)
    .await
    .map_err(db)?;
    Ok(())
}

fn plain_row(
    kind: &str,
    name: String,
    value: Vec<u8>,
    at: DateTime<Utc>,
    label: Option<String>,
) -> StateRow {
    StateRow {
        kind: kind.to_owned(),
        scope: store::OWN.to_owned(),
        name,
        value,
        sealed: false,
        updated: rfc3339(at),
        label,
    }
}

async fn state_get(
    c: &mut PgConnection,
    key: &Option<Arc<Key>>,
    kind: &str,
    name: &str,
) -> Result<Option<StateRow>> {
    Ok(match spot(kind) {
        Spot::Plain(place) => place.get(c, kind, key, name).await?,
        Spot::Tailnet => column_get(c, "hosts", "tailnet", Some(name))
            .await?
            .pop()
            .map(|(n, v, at)| plain_row(kind, n, v.into_bytes(), at, None)),
        Spot::Resource(col) => column_get(c, "resources", col, Some(name))
            .await?
            .pop()
            .map(|(n, v, at)| plain_row(kind, n, v.into_bytes(), at, None)),
        Spot::Named(plugin) => named_get(c, key, plugin, Some(name))
            .await?
            .pop()
            .map(|(n, v, at)| plain_row(kind, n, v, at, None)),
        Spot::Secret => match SECRETS.get(c, kind, key, name).await? {
            Some(row) => Some(row),
            None => named_get(c, key, OWNED, Some(name))
                .await?
                .pop()
                .map(|(n, v, at)| plain_row(kind, n, v, at, None)),
        },
        Spot::None => None,
    })
}

async fn state_all(
    c: &mut PgConnection,
    key: &Option<Arc<Key>>,
    kind: &str,
) -> Result<Vec<StateRow>> {
    Ok(match spot(kind) {
        Spot::Plain(place) => place.all(c, kind, key).await?,
        Spot::Tailnet => column_get(c, "hosts", "tailnet", None)
            .await?
            .into_iter()
            .map(|(n, v, at)| plain_row(kind, n, v.into_bytes(), at, None))
            .collect(),
        Spot::Resource(col) => column_get(c, "resources", col, None)
            .await?
            .into_iter()
            .map(|(n, v, at)| plain_row(kind, n, v.into_bytes(), at, None))
            .collect(),
        Spot::Named(plugin) => named_get(c, key, plugin, None)
            .await?
            .into_iter()
            .map(|(n, v, at)| plain_row(kind, n, v, at, None))
            .collect(),
        Spot::Secret => {
            let mut rows = SECRETS.all(c, kind, key).await?;
            rows.extend(
                named_get(c, key, OWNED, None)
                    .await?
                    .into_iter()
                    .map(|(n, v, at)| plain_row(kind, n, v, at, None)),
            );
            rows.sort_by(|a, b| a.name.cmp(&b.name));
            rows
        }
        Spot::None => vec![],
    })
}

async fn state_put(c: &mut PgConnection, key: &Option<Arc<Key>>, row: &StateRow) -> Result<()> {
    own(&row.scope)?;
    let kind = row.kind.as_str();
    match spot(kind) {
        Spot::Plain(place) => place.put(c, key, row).await,
        Spot::Tailnet => {
            json_of(kind, &row.name, &row.value)?;
            let done = sqlx::query(
                "UPDATE rookery.hosts SET tailnet = $2::jsonb, updated_at = now() WHERE id = $1",
            )
            .bind(&row.name)
            .bind(String::from_utf8_lossy(&row.value).into_owned())
            .execute(&mut *c)
            .await
            .map_err(db)?;
            match done.rows_affected() {
                0 => Err(Error::Given(format!("no host has the id {}", row.name))),
                _ => Ok(()),
            }
        }
        Spot::Resource(col) => {
            json_of(kind, &row.name, &row.value)?;
            let sql = format!(
                "INSERT INTO rookery.resources (id, {col}) VALUES ($1, $2::jsonb)
                 ON CONFLICT (id) DO UPDATE SET {col} = EXCLUDED.{col}, updated_at = now()"
            );
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(&row.name)
                .bind(String::from_utf8_lossy(&row.value).into_owned())
                .execute(&mut *c)
                .await
                .map_err(db)?;
            Ok(())
        }
        Spot::Named(plugin) => named_put(c, key, plugin, &row.name, &row.value).await,
        Spot::Secret => {
            // A secret with a name of its own is kept by its id; one that belongs to a thing by its
            // name of rookery-core's, `ID/NAME`.
            let labelled = row.label.is_some()
                || (Uuid::parse_str(&row.name).is_ok()
                    && SECRETS.get(c, kind, &None, &row.name).await.is_ok_and(|r| r.is_some())
                    || Uuid::parse_str(&row.name).is_ok()
                        && sqlx::query_scalar::<_, i64>(
                            "SELECT count(*) FROM rookery.secrets WHERE id = $1::uuid AND plugin = 'rookery'",
                        )
                        .bind(&row.name)
                        .fetch_one(&mut *c)
                        .await
                        .map_err(db)?
                            > 0);
            match labelled {
                true => SECRETS.put(c, key, row).await,
                false => named_put(c, key, OWNED, &row.name, &row.value).await,
            }
        }
        Spot::None => Err(not_kept(kind)),
    }
}

async fn state_remove(c: &mut PgConnection, kind: &str, name: &str) -> Result<bool> {
    Ok(match spot(kind) {
        Spot::Plain(place) => place.remove(c, name).await?,
        Spot::Tailnet => {
            sqlx::query(
                "UPDATE rookery.hosts SET tailnet = NULL WHERE id = $1 AND tailnet IS NOT NULL",
            )
            .bind(name)
            .execute(&mut *c)
            .await
            .map_err(db)?
            .rows_affected()
                > 0
        }
        Spot::Resource(col) => {
            let sql = format!(
                "UPDATE rookery.resources SET {col} = NULL WHERE id = $1 AND {col} IS NOT NULL"
            );
            // The row goes when neither part is left, which the table's check would refuse.
            let other = if col == "record" { "spec" } else { "record" };
            let gone = sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM rookery.resources WHERE id = $1 AND {col} IS NOT NULL AND {other} IS NULL"
            )))
            .bind(name)
            .execute(&mut *c)
            .await
            .map_err(db)?
            .rows_affected();
            gone > 0
                || sqlx::query(sqlx::AssertSqlSafe(sql))
                    .bind(name)
                    .execute(&mut *c)
                    .await
                    .map_err(db)?
                    .rows_affected()
                    > 0
        }
        Spot::Named(plugin) => named_remove(c, plugin, name).await?,
        Spot::Secret => SECRETS.remove(c, name).await? || named_remove(c, OWNED, name).await?,
        Spot::None => false,
    })
}

async fn state_clear(c: &mut PgConnection, kind: &str) -> Result<()> {
    match spot(kind) {
        Spot::Plain(place) => place.clear(c).await,
        Spot::Tailnet => {
            sqlx::query("UPDATE rookery.hosts SET tailnet = NULL")
                .execute(&mut *c)
                .await
                .map_err(db)?;
            Ok(())
        }
        Spot::Resource(_) | Spot::Named(_) | Spot::Secret | Spot::None => Err(not_kept(kind)),
    }
}

async fn forget_owned(c: &mut PgConnection, owner: &str) -> Result<()> {
    for place in [ACCESS, ARGUMENTS, PACKAGES, SETTINGS] {
        place.forget_owned(c, owner).await?;
    }
    sqlx::query("UPDATE rookery.hosts SET tailnet = NULL WHERE id = $1")
        .bind(owner)
        .execute(&mut *c)
        .await
        .map_err(db)?;
    sqlx::query("DELETE FROM rookery.resources WHERE id = $1 OR starts_with(id, $1 || '/')")
        .bind(owner)
        .execute(&mut *c)
        .await
        .map_err(db)?;
    named_forget_owned(c, OWNED, owner).await?;
    named_forget_owned(c, CREDENTIALS, owner).await?;
    Ok(())
}

/// A label another thing of the kind has: the error rookery-core's store turns into its own.
fn taken(kind: &str, label: Option<&str>, error: Error) -> Error {
    match error {
        Error::Taken { .. } => Error::Taken {
            kind: kind.to_owned(),
            label: label.unwrap_or_default().to_owned(),
        },
        other => other,
    }
}

impl Backend for ServerBackend {
    fn name(&self) -> &'static str {
        "postgres"
    }

    fn schema_version(&mut self) -> Result<i64> {
        Ok(rookery_store::migrations().len() as i64)
    }

    fn cursor(&mut self, host: &str) -> Result<u64> {
        let host = host.to_owned();
        self.tx(move |c, _| {
            Box::pin(async move {
                let id: Option<i64> =
                    sqlx::query_scalar("SELECT last_id FROM rookery.host_cursors WHERE host = $1")
                        .bind(&host)
                        .fetch_optional(&mut *c)
                        .await
                        .map_err(db)?;
                Ok(id.unwrap_or(0) as u64)
            })
        })
    }

    fn take_in(&mut self, host: &str, message: &Message) -> Result<Option<Stored>> {
        let (host, message, fence) = (host.to_owned(), message.clone(), self.fence);
        self.tx(move |c, _| {
            Box::pin(async move {
                // A server that does not lead any more takes in nothing.
                sqlx::query("SELECT rookery.fence($1)")
                    .bind(fence)
                    .execute(&mut *c)
                    .await
                    .map_err(|e| Error::Database(format!("taking in a message: {e}")))?;
                take_in(c, &host, &message).await
            })
        })
    }

    fn reset_cursor(&mut self, host: &str) -> Result<()> {
        let host = host.to_owned();
        self.tx(move |c, _| {
            Box::pin(async move {
                sqlx::query("DELETE FROM rookery.host_cursors WHERE host = $1")
                    .bind(&host)
                    .execute(&mut *c)
                    .await
                    .map_err(db)?;
                Ok(())
            })
        })
    }

    fn inbox(&mut self, filter: &Filter<'_>, limit: usize) -> Result<Vec<Stored>> {
        let host = filter.host.map(str::to_owned);
        let topic = filter.topic.unwrap_or("").to_owned();
        let (before, done) = (filter.before, filter.done);
        self.tx(move |c, _| {
            Box::pin(async move {
                let rows = sqlx::query(
                    "SELECT seq, host, id, topic, body::text AS body, keep, from_level, from_uid, session, time, done
                     FROM rookery.host_messages
                     WHERE ($1::text IS NULL OR host = $1) AND starts_with(topic, $2)
                       AND ($3::bigint IS NULL OR seq < $3) AND ($4::boolean IS NULL OR done = $4)
                     ORDER BY seq DESC LIMIT $5",
                )
                .bind(host)
                .bind(topic)
                .bind(before)
                .bind(done)
                .bind(limit as i64)
                .fetch_all(&mut *c)
                .await
                .map_err(db)?;
                Ok(rows.iter().map(stored).collect())
            })
        })
    }

    fn set_done(&mut self, host: &str, id: u64, done: bool) -> Result<bool> {
        let host = host.to_owned();
        self.tx(move |c, _| {
            Box::pin(async move {
                let changed = sqlx::query(
                    "UPDATE rookery.host_messages SET done = $3 WHERE host = $1 AND id = $2",
                )
                .bind(&host)
                .bind(id as i64)
                .bind(done)
                .execute(&mut *c)
                .await
                .map_err(db)?;
                Ok(changed.rows_affected() == 1)
            })
        })
    }

    fn get(&mut self, namespace: &str, key: &str) -> Result<Option<Entry>> {
        let (namespace, key) = (namespace.to_owned(), key.to_owned());
        self.tx(move |c, _| {
            Box::pin(async move {
                let row: Option<(String, Value, i64, DateTime<Utc>)> = sqlx::query_as(
                    "SELECT key, value, revision, updated_at FROM rookery.kv WHERE plugin = $1 AND scope = 'global' AND user_id IS NULL AND key = $2",
                )
                .bind(&namespace)
                .bind(&key)
                .fetch_optional(&mut *c)
                .await
                .map_err(db)?;
                Ok(row.map(entry))
            })
        })
    }

    fn put(
        &mut self,
        namespace: &str,
        key: &str,
        text: &str,
        when: When,
        _updated: &str,
    ) -> Result<i64> {
        let (namespace, key) = (namespace.to_owned(), key.to_owned());
        let value: Value = serde_json::from_str(text).map_err(|e| Error::Given(e.to_string()))?;
        self.tx(move |c, _| {
            Box::pin(async move {
                let current: Option<i64> = sqlx::query_scalar(
                    "SELECT revision FROM rookery.kv WHERE plugin = $1 AND scope = 'global' AND user_id IS NULL AND key = $2 FOR UPDATE",
                )
                .bind(&namespace)
                .bind(&key)
                .fetch_optional(&mut *c)
                .await
                .map_err(db)?;
                let allowed = match when {
                    When::Always => true,
                    When::At(revision) => current == Some(revision),
                    When::New => current.is_none(),
                };
                if !allowed {
                    return Err(Error::Conflict(current));
                }
                let revision = current.unwrap_or(0) + 1;
                sqlx::query(
                    "INSERT INTO rookery.kv (plugin, scope, user_id, key, value, revision) VALUES ($1, 'global', NULL, $2, $3, $4)
                     ON CONFLICT (plugin, scope, user_id, key) DO UPDATE SET value = EXCLUDED.value, revision = EXCLUDED.revision, updated_at = now()",
                )
                .bind(&namespace)
                .bind(&key)
                .bind(&value)
                .bind(revision)
                .execute(&mut *c)
                .await
                .map_err(db)?;
                Ok(revision)
            })
        })
    }

    fn remove(&mut self, namespace: &str, key: &str, when: When) -> Result<bool> {
        let (namespace, key) = (namespace.to_owned(), key.to_owned());
        self.tx(move |c, _| {
            Box::pin(async move {
                let current: Option<i64> = sqlx::query_scalar(
                    "SELECT revision FROM rookery.kv WHERE plugin = $1 AND scope = 'global' AND user_id IS NULL AND key = $2 FOR UPDATE",
                )
                .bind(&namespace)
                .bind(&key)
                .fetch_optional(&mut *c)
                .await
                .map_err(db)?;
                if let When::At(revision) = when
                    && current != Some(revision)
                {
                    return Err(Error::Conflict(current));
                }
                let done = sqlx::query(
                    "DELETE FROM rookery.kv WHERE plugin = $1 AND scope = 'global' AND user_id IS NULL AND key = $2",
                )
                .bind(&namespace)
                .bind(&key)
                .execute(&mut *c)
                .await
                .map_err(db)?;
                Ok(done.rows_affected() == 1)
            })
        })
    }

    fn list(&mut self, namespace: &str) -> Result<Vec<Entry>> {
        let namespace = namespace.to_owned();
        self.tx(move |c, _| {
            Box::pin(async move {
                let rows: Vec<(String, Value, i64, DateTime<Utc>)> = sqlx::query_as(
                    "SELECT key, value, revision, updated_at FROM rookery.kv WHERE plugin = $1 AND scope = 'global' AND user_id IS NULL ORDER BY key",
                )
                .bind(&namespace)
                .fetch_all(&mut *c)
                .await
                .map_err(db)?;
                Ok(rows.into_iter().map(entry).collect())
            })
        })
    }

    // Jobs are rookery.jobs, which the server's runner writes through rookery-jobs.
    fn add_job(&mut self, _: &Job) -> Result<Job> {
        Err(not_kept("job records of its own: jobs are rookery.jobs"))
    }
    fn job_by_token(&mut self, _: &str, _: &str, _: &str) -> Result<Option<Job>> {
        Ok(None)
    }
    fn end_job(&mut self, _: &str, _: Option<i32>, _: &str) -> Result<()> {
        Ok(())
    }
    fn stop_job(&mut self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
    fn end_lost_jobs(&mut self, _: &str) -> Result<usize> {
        Ok(0)
    }
    fn job(&mut self, _: &str) -> Result<Option<Job>> {
        Ok(None)
    }
    fn jobs(&mut self, _: &JobFilter<'_>) -> Result<Vec<Job>> {
        Ok(vec![])
    }

    fn prune(&mut self, cutoff: &str) -> Result<Vec<String>> {
        let cutoff = cutoff.to_owned();
        self.tx(move |c, _| {
            Box::pin(async move {
                sqlx::query("DELETE FROM rookery.host_messages WHERE time < $1")
                    .bind(&cutoff)
                    .execute(&mut *c)
                    .await
                    .map_err(db)?;
                Ok(vec![])
            })
        })
    }

    fn state(&mut self, kind: &str, scope: &str, name: &str) -> Result<Option<StateRow>> {
        if scope != store::OWN {
            return Ok(None);
        }
        let (kind, name) = (kind.to_owned(), name.to_owned());
        self.tx(move |c, key| Box::pin(async move { state_get(c, &key, &kind, &name).await }))
    }

    fn states(&mut self, kind: &str, scope: &str) -> Result<Vec<StateRow>> {
        if scope != store::OWN {
            return Ok(vec![]);
        }
        let kind = kind.to_owned();
        self.tx(move |c, key| Box::pin(async move { state_all(c, &key, &kind).await }))
    }

    fn put_state(&mut self, row: &StateRow) -> Result<()> {
        let row = row.clone();
        let (kind, label) = (row.kind.clone(), row.label.clone());
        self.tx(move |c, key| Box::pin(async move { state_put(c, &key, &row).await }))
            .map_err(|e| taken(&kind, label.as_deref(), e))
    }

    fn remove_state(&mut self, kind: &str, scope: &str, name: &str) -> Result<bool> {
        if scope != store::OWN {
            return Ok(false);
        }
        let (kind, name) = (kind.to_owned(), name.to_owned());
        self.tx(move |c, _| Box::pin(async move { state_remove(c, &kind, &name).await }))
    }

    fn replace_states(&mut self, scope: &str, kinds: &[&str], rows: &[StateRow]) -> Result<()> {
        own(scope)?;
        let kinds: Vec<String> = kinds.iter().map(|k| (*k).to_owned()).collect();
        let rows = rows.to_vec();
        self.tx(move |c, key| {
            Box::pin(async move {
                for kind in &kinds {
                    state_clear(c, kind).await?;
                }
                for row in &rows {
                    state_put(c, &key, row).await?;
                }
                Ok(())
            })
        })
    }

    fn all_states(&mut self) -> Result<Vec<StateRow>> {
        self.tx(move |c, key| {
            Box::pin(async move {
                let mut all = vec![];
                for kind in KINDS {
                    all.extend(state_all(c, &key, kind).await?);
                }
                Ok(all)
            })
        })
    }

    fn labelled(&mut self, kind: &str, scope: &str, label: &str) -> Result<Option<StateRow>> {
        if scope != store::OWN {
            return Ok(None);
        }
        let (kind, label) = (kind.to_owned(), label.to_owned());
        self.tx(move |c, key| {
            Box::pin(async move {
                match spot(&kind) {
                    Spot::Plain(place) => place.labelled(c, &kind, &key, &label).await,
                    Spot::Secret => SECRETS.labelled(c, &kind, &key, &label).await,
                    _ => Ok(None),
                }
            })
        })
    }

    fn relabel(
        &mut self,
        kind: &str,
        scope: &str,
        name: &str,
        label: &str,
        _updated: &str,
    ) -> Result<bool> {
        if scope != store::OWN {
            return Ok(false);
        }
        let (kind, name, label) = (kind.to_owned(), name.to_owned(), label.to_owned());
        let (k, l) = (kind.clone(), label.clone());
        self.tx(move |c, _| {
            Box::pin(async move {
                match spot(&kind) {
                    Spot::Plain(place) => place.relabel(c, &name, &label).await,
                    Spot::Secret => SECRETS.relabel(c, &name, &label).await,
                    _ => Ok(false),
                }
            })
        })
        .map_err(|e| taken(&k, Some(&l), e))
    }

    fn change_states(&mut self, change: &Change) -> Result<()> {
        let change = change.clone();
        self.tx(move |c, key| {
            Box::pin(async move {
                for (kind, scope, name) in &change.removals {
                    if scope == store::OWN {
                        state_remove(c, kind, name).await?;
                    }
                }
                for (scope, owner) in &change.owners {
                    if scope == store::OWN {
                        forget_owned(c, owner).await?;
                    }
                }
                for row in &change.writes {
                    state_put(c, &key, row)
                        .await
                        .map_err(|e| taken(&row.kind, row.label.as_deref(), e))?;
                }
                for (old, new) in &change.hosts {
                    for sql in [
                        "UPDATE rookery.host_messages SET host = $2 WHERE host = $1",
                        "UPDATE rookery.host_cursors SET host = $2 WHERE host = $1",
                    ] {
                        sqlx::query(sql)
                            .bind(old)
                            .bind(new)
                            .execute(&mut *c)
                            .await
                            .map_err(db)?;
                    }
                }
                // A job's subject is a resource URI the server keeps; rookery-core's are not moved.
                Ok(())
            })
        })
    }

    fn swap_states(&mut self, _: &[(StateRow, StateRow)]) -> Result<bool> {
        Err(not_kept(
            "store sealed as a whole: rookery server secrets rekey seals its secrets again",
        ))
    }

    fn export(&mut self, _: Table) -> Result<Rows> {
        Err(not_kept("generic tables"))
    }

    fn import(&mut self, _: &Rows) -> Result<usize> {
        Err(not_kept("generic tables"))
    }

    fn count(&mut self, _: Table) -> Result<u64> {
        Ok(0)
    }
}

fn entry((key, value, revision, at): (String, Value, i64, DateTime<Utc>)) -> Entry {
    Entry {
        key,
        value,
        revision,
        updated: rfc3339(at),
    }
}

fn stored(r: &sqlx::postgres::PgRow) -> Stored {
    let level = match r.get::<String, _>("from_level").as_str() {
        "agent" => MessageLevel::Agent,
        _ => MessageLevel::Host,
    };
    let keep = match r.get::<String, _>("keep").as_str() {
        "last" => Keep::Last,
        _ => Keep::Log,
    };
    Stored {
        seq: r.get("seq"),
        host: r.get("host"),
        done: r.get("done"),
        message: Message {
            id: r.get::<i64, _>("id") as u64,
            topic: r.get("topic"),
            body: serde_json::from_str(&r.get::<String, _>("body")).unwrap_or(Value::Null),
            keep,
            from: MessageFrom {
                level,
                uid: r.get::<i64, _>("from_uid") as u32,
                session: r.get("session"),
            },
            time: r.get("time"),
        },
    }
}

async fn take_in(c: &mut PgConnection, host: &str, message: &Message) -> Result<Option<Stored>> {
    let mut taken = None;
    if message.keep != Keep::None {
        if message.keep == Keep::Last {
            sqlx::query(
                "DELETE FROM rookery.host_messages WHERE host = $1 AND topic = $2 AND keep = 'last' AND id < $3",
            )
            .bind(host)
            .bind(&message.topic)
            .bind(message.id as i64)
            .execute(&mut *c)
            .await
            .map_err(db)?;
        }
        let level = match message.from.level {
            MessageLevel::Host => "host",
            MessageLevel::Agent => "agent",
        };
        let keep = match message.keep {
            Keep::Last => "last",
            _ => "log",
        };
        let seq: Option<i64> = sqlx::query_scalar(
            "INSERT INTO rookery.host_messages (host, id, topic, body, keep, from_level, from_uid, session, time)
             VALUES ($1, $2, $3, $4::jsonb, $5, $6, $7, $8, $9) ON CONFLICT (host, id) DO NOTHING RETURNING seq",
        )
        .bind(host)
        .bind(message.id as i64)
        .bind(&message.topic)
        .bind(message.body.to_string())
        .bind(keep)
        .bind(level)
        .bind(message.from.uid as i64)
        .bind(&message.from.session)
        .bind(&message.time)
        .fetch_optional(&mut *c)
        .await
        .map_err(db)?;
        taken = seq.map(|seq| Stored {
            seq,
            host: host.to_owned(),
            message: message.clone(),
            done: false,
        });
    }
    sqlx::query(
        "INSERT INTO rookery.host_cursors (host, last_id) VALUES ($1, $2)
         ON CONFLICT (host) DO UPDATE SET last_id = greatest(rookery.host_cursors.last_id, EXCLUDED.last_id)",
    )
    .bind(host)
    .bind(message.id as i64)
    .execute(&mut *c)
    .await
    .map_err(db)?;
    Ok(taken)
}

#[allow(dead_code)]
fn bare_id(id: Uuid) -> String {
    bare(id)
}
