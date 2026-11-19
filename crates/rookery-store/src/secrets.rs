//! Secrets of plugins, sealed in their rows. A server with no key refuses them with `UNAVAILABLE`.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::Store;
use crate::error::{Error, Result};
use crate::seal::{Key, Sealed, row};
use crate::util::new_id;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    User(Uuid),
}

impl Scope {
    pub fn parts(self) -> (&'static str, Option<Uuid>) {
        match self {
            Scope::Global => ("global", None),
            Scope::User(user) => ("user", Some(user)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Info {
    pub id: Uuid,
    pub plugin: String,
    pub name: String,
    pub scope: String,
    pub user_id: Option<Uuid>,
    pub key_id: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct SealedRow {
    id: Uuid,
    key_id: String,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

/// How many rows a rekey moved to the new key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rekeyed {
    pub secrets: u64,
    pub connections: u64,
}

pub struct Secrets<'a> {
    pub(crate) store: &'a Store,
}

const WHERE: &str = "plugin = $1 AND name = $2 AND scope = $3 AND user_id IS NOT DISTINCT FROM $4";

fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty() && name.len() <= 200 && !name.chars().any(char::is_control);
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(
            "a secret's name is 1 to 200 characters".into(),
        ))
    }
}

impl Secrets<'_> {
    /// Sets a secret, new or changed.
    pub async fn set(&self, plugin: &str, name: &str, scope: Scope, value: &[u8]) -> Result<Info> {
        check_name(name)?;
        let key = self.store.key()?;
        let (scope_name, user) = scope.parts();
        let mut tx = self.store.pool().begin().await?;
        let found: Option<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT id FROM rookery.secrets WHERE {WHERE} FOR UPDATE"
        )))
        .bind(plugin)
        .bind(name)
        .bind(scope_name)
        .bind(user)
        .fetch_optional(&mut *tx)
        .await?;
        let id = found.unwrap_or_else(new_id);
        let sealed = key.seal(&row("secrets", id), value)?;
        let info: Info = sqlx::query_as(
            "INSERT INTO rookery.secrets (id, plugin, name, scope, user_id, key_id, nonce, ciphertext) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (id) DO UPDATE SET key_id = EXCLUDED.key_id, nonce = EXCLUDED.nonce, ciphertext = EXCLUDED.ciphertext, updated_at = now()
             RETURNING id, plugin, name, scope, user_id, key_id, updated_at",
        )
        .bind(id)
        .bind(plugin)
        .bind(name)
        .bind(scope_name)
        .bind(user)
        .bind(&sealed.key_id)
        .bind(&sealed.nonce)
        .bind(&sealed.ciphertext)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match Error::from(e) {
            e if e.is_unique_violation() => Error::Conflict("the secret was set at the same moment; set it again".into()),
            e => e,
        })?;
        tx.commit().await?;
        Ok(info)
    }

    pub async fn get(&self, plugin: &str, name: &str, scope: Scope) -> Result<Option<Vec<u8>>> {
        let key = self.store.key()?;
        let (scope_name, user) = scope.parts();
        let found: Option<SealedRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT id, key_id, nonce, ciphertext FROM rookery.secrets WHERE {WHERE}"
        )))
        .bind(plugin)
        .bind(name)
        .bind(scope_name)
        .bind(user)
        .fetch_optional(self.store.pool())
        .await?;
        match found {
            Some(r) => Ok(Some(key.open(
                &row("secrets", r.id),
                &Sealed {
                    key_id: r.key_id,
                    nonce: r.nonce,
                    ciphertext: r.ciphertext,
                },
            )?)),
            None => Ok(None),
        }
    }

    /// The secrets a list may show, without their values: of one plugin or of all, global ones and those
    /// of `user`.
    pub async fn list(&self, plugin: Option<&str>, user: Option<Uuid>) -> Result<Vec<Info>> {
        Ok(sqlx::query_as(
            "SELECT id, plugin, name, scope, user_id, key_id, updated_at FROM rookery.secrets
             WHERE ($1::text IS NULL OR plugin = $1) AND (scope = 'global' OR user_id = $2)
             ORDER BY plugin, name, scope",
        )
        .bind(plugin)
        .bind(user)
        .fetch_all(self.store.pool())
        .await?)
    }

    pub async fn delete(&self, plugin: &str, name: &str, scope: Scope) -> Result<bool> {
        let (scope_name, user) = scope.parts();
        let done = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM rookery.secrets WHERE {WHERE}"
        )))
        .bind(plugin)
        .bind(name)
        .bind(scope_name)
        .bind(user)
        .execute(self.store.pool())
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Gives a secret another name. Its sealed value names its row, not its name, so it stays as it is.
    pub async fn rename(&self, plugin: &str, name: &str, scope: Scope, new: &str) -> Result<()> {
        check_name(new)?;
        let (scope_name, user) = scope.parts();
        let done = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE rookery.secrets SET name = $5, updated_at = now() WHERE {WHERE}"
        )))
        .bind(plugin)
        .bind(name)
        .bind(scope_name)
        .bind(user)
        .bind(new)
        .execute(self.store.pool())
        .await
        .map_err(|e| match Error::from(e) {
            e if e.is_unique_violation() => {
                Error::Conflict(format!("there is a secret {new} already"))
            }
            e => e,
        })?;
        if done.rows_affected() == 0 {
            return Err(Error::NotFound(format!("no secret {name}")));
        }
        Ok(())
    }

    /// Seals every secret and every connection's credential again under `new`, in one transaction. Rows
    /// already under `new` are left as they are; a row under neither key stops it.
    pub async fn rekey(&self, old: &Key, new: &Key) -> Result<Rekeyed> {
        let mut tx = self.store.pool().begin().await?;
        let mut done = Rekeyed::default();
        for (table, count) in [
            ("secrets", &mut done.secrets),
            ("connections", &mut done.connections),
        ] {
            let rows: Vec<SealedRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT id, key_id, nonce, ciphertext FROM rookery.{table} WHERE key_id IS NOT NULL AND key_id <> $1 FOR UPDATE"
            )))
            .bind(new.id())
            .fetch_all(&mut *tx)
            .await?;
            for r in rows {
                let aad = row(table, r.id);
                let plain = old.open(
                    &aad,
                    &Sealed {
                        key_id: r.key_id,
                        nonce: r.nonce,
                        ciphertext: r.ciphertext,
                    },
                )?;
                let sealed = new.seal(&aad, &plain)?;
                sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE rookery.{table} SET key_id = $2, nonce = $3, ciphertext = $4 WHERE id = $1")))
                    .bind(r.id)
                    .bind(&sealed.key_id)
                    .bind(&sealed.nonce)
                    .bind(&sealed.ciphertext)
                    .execute(&mut *tx)
                    .await?;
                *count += 1;
            }
        }
        tx.commit().await?;
        Ok(done)
    }
}
