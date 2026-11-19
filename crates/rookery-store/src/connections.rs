//! Connections: credentials of outside services, such as a registry and its login. The config is
//! plain; the credential is sealed in the row like a secret.

use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use crate::Store;
use crate::error::{Error, Result};
use crate::seal::{Sealed, row};
use crate::util::new_id;

/// Whose a connection is: everyone's, a space's members', or one person's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Space(Uuid),
    User(Uuid),
}

impl From<crate::secrets::Scope> for Scope {
    fn from(s: crate::secrets::Scope) -> Self {
        match s {
            crate::secrets::Scope::Global => Scope::Global,
            crate::secrets::Scope::User(u) => Scope::User(u),
        }
    }
}

impl Scope {
    fn parts(self) -> (&'static str, Option<Uuid>, Option<Uuid>) {
        match self {
            Scope::Global => ("global", None, None),
            Scope::Space(s) => ("space", None, Some(s)),
            Scope::User(u) => ("user", Some(u), None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Connection {
    pub id: Uuid,
    #[sqlx(rename = "type")]
    pub kind: String,
    pub name: String,
    pub scope: String,
    pub user_id: Option<Uuid>,
    pub space_id: Option<Uuid>,
    pub config: Value,
    /// Whether it has a credential.
    pub sealed: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const COLS: &str = "id, type, name, scope, user_id, space_id, config, ciphertext IS NOT NULL AS sealed, created_by, created_at, updated_at";

/// A change to a connection: what is `Some` changes. A credential of `Some(None)` removes it.
#[derive(Debug, Clone, Default)]
pub struct Change<'a> {
    pub name: Option<&'a str>,
    pub config: Option<&'a Value>,
    pub credential: Option<Option<&'a [u8]>>,
}

pub struct Connections<'a> {
    pub(crate) store: &'a Store,
}

fn unique(e: sqlx::Error) -> Error {
    match Error::from(e) {
        e if e.is_unique_violation() => {
            Error::Conflict("there is a connection of that type with that name".into())
        }
        e => e,
    }
}

impl Connections<'_> {
    fn seal(&self, id: Uuid, credential: Option<&[u8]>) -> Result<Option<Sealed>> {
        match credential {
            Some(plain) => Ok(Some(
                self.store.key()?.seal(&row("connections", id), plain)?,
            )),
            None => Ok(None),
        }
    }

    pub async fn create(
        &self,
        kind: &str,
        name: &str,
        scope: impl Into<Scope>,
        config: &Value,
        credential: Option<&[u8]>,
        by: &str,
    ) -> Result<Connection> {
        if name.trim().is_empty() || name.len() > 200 {
            return Err(Error::Invalid(
                "a connection's name is 1 to 200 characters".into(),
            ));
        }
        let id = new_id();
        let sealed = self.seal(id, credential)?;
        let (scope, user, space) = scope.into().parts();
        let mut tx = crate::actor::begin_as(self.store.pool(), by).await?;
        let made = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "INSERT INTO rookery.connections (id, type, name, scope, user_id, space_id, config, key_id, nonce, ciphertext, created_by)
             VALUES ($1, $2, $3, $4, $5, $11, $6, $7, $8, $9, $10) RETURNING {COLS}"
        )))
        .bind(id)
        .bind(kind)
        .bind(name)
        .bind(scope)
        .bind(user)
        .bind(config)
        .bind(sealed.as_ref().map(|s| s.key_id.clone()))
        .bind(sealed.as_ref().map(|s| s.nonce.clone()))
        .bind(sealed.as_ref().map(|s| s.ciphertext.clone()))
        .bind(by)
        .bind(space)
        .fetch_one(&mut *tx)
        .await
        .map_err(unique)?;
        tx.commit().await?;
        Ok(made)
    }

    /// The global connections, those of `user` and those of `spaces`.
    pub async fn list_in(
        &self,
        kind: Option<&str>,
        user: Option<Uuid>,
        spaces: &[Uuid],
    ) -> Result<Vec<Connection>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLS} FROM rookery.connections WHERE ($1::text IS NULL OR type = $1)
                 AND (scope = 'global' OR user_id = $2 OR space_id = ANY ($3))
             ORDER BY type, lower(name), scope"
        )))
        .bind(kind)
        .bind(user)
        .bind(spaces)
        .fetch_all(self.store.pool())
        .await?)
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<Connection>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLS} FROM rookery.connections WHERE id = $1"
        )))
        .bind(id)
        .fetch_optional(self.store.pool())
        .await?)
    }

    /// The connection of a type and name a user sees: their own first, then the global one.
    pub async fn find(
        &self,
        kind: &str,
        name: &str,
        user: Option<Uuid>,
    ) -> Result<Option<Connection>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLS} FROM rookery.connections WHERE type = $1 AND name = $2 AND (scope = 'global' OR user_id = $3)
             ORDER BY scope = 'user' DESC LIMIT 1"
        )))
        .bind(kind)
        .bind(name)
        .bind(user)
        .fetch_optional(self.store.pool())
        .await?)
    }

    /// The connection of a type and name for a person in a space: theirs first, then the space's, then the
    /// global one.
    pub async fn find_in(
        &self,
        kind: &str,
        name: &str,
        user: Option<Uuid>,
        space: Option<Uuid>,
    ) -> Result<Option<Connection>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLS} FROM rookery.connections WHERE type = $1 AND name = $2
                 AND (scope = 'global' OR user_id = $3 OR space_id = $4)
             ORDER BY CASE WHEN user_id IS NOT NULL THEN 0 WHEN space_id IS NOT NULL THEN 1 ELSE 2 END LIMIT 1"
        )))
        .bind(kind)
        .bind(name)
        .bind(user)
        .bind(space)
        .fetch_optional(self.store.pool())
        .await?)
    }

    /// The global connections, and those of `user`.
    pub async fn list(&self, kind: Option<&str>, user: Option<Uuid>) -> Result<Vec<Connection>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLS} FROM rookery.connections WHERE ($1::text IS NULL OR type = $1) AND (scope = 'global' OR user_id = $2)
             ORDER BY type, lower(name), scope"
        )))
        .bind(kind)
        .bind(user)
        .fetch_all(self.store.pool())
        .await?)
    }

    pub async fn update(&self, id: Uuid, change: Change<'_>) -> Result<Connection> {
        let credential = match change.credential {
            Some(credential) => Some(self.seal(id, credential)?),
            None => None,
        };
        let sealed = credential.as_ref().and_then(|c| c.as_ref());
        let mut tx = crate::actor::begin(self.store.pool()).await?;
        let changed = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE rookery.connections SET
                 name = coalesce($2, name),
                 config = coalesce($3, config),
                 key_id = CASE WHEN $4 THEN $5 ELSE key_id END,
                 nonce = CASE WHEN $4 THEN $6 ELSE nonce END,
                 ciphertext = CASE WHEN $4 THEN $7 ELSE ciphertext END,
                 updated_at = now()
             WHERE id = $1 RETURNING {COLS}"
        )))
        .bind(id)
        .bind(change.name)
        .bind(change.config)
        .bind(credential.is_some())
        .bind(sealed.map(|s| s.key_id.clone()))
        .bind(sealed.map(|s| s.nonce.clone()))
        .bind(sealed.map(|s| s.ciphertext.clone()))
        .fetch_optional(&mut *tx)
        .await
        .map_err(unique)?
        .ok_or_else(|| Error::NotFound("no such connection".into()))?;
        tx.commit().await?;
        Ok(changed)
    }

    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        let mut tx = crate::actor::begin(self.store.pool()).await?;
        let done = sqlx::query("DELETE FROM rookery.connections WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(done.rows_affected() > 0)
    }

    /// The opened credential of a connection, if it has one.
    pub async fn credential(&self, id: Uuid) -> Result<Option<Vec<u8>>> {
        type Row = (Option<String>, Option<Vec<u8>>, Option<Vec<u8>>);
        let found: Option<Row> = sqlx::query_as(
            "SELECT key_id, nonce, ciphertext FROM rookery.connections WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.store.pool())
        .await?;
        match found {
            None => Err(Error::NotFound("no such connection".into())),
            Some((Some(key_id), Some(nonce), Some(ciphertext))) => {
                Ok(Some(self.store.key()?.open(
                    &row("connections", id),
                    &Sealed {
                        key_id,
                        nonce,
                        ciphertext,
                    },
                )?))
            }
            Some(_) => Ok(None),
        }
    }
}
