//! Device sessions and access tokens. A token is random, shown once and kept as its SHA-256.

use chrono::{DateTime, Utc};
use sqlx::AssertSqlSafe;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::users::{User, Users};
use crate::util::{hash_token, hint, new_id, random_token};

/// What a device session's token starts with.
pub const SESSION_PREFIX: &str = "rookery_ses_";
/// What an access token starts with.
pub const TOKEN_PREFIX: &str = "rookery_pat_";

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct DeviceSession {
    pub id: Uuid,
    pub user_id: Uuid,
    pub label: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct AccessToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub hint: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

const SESSION_COLS: &str = "s.id, s.user_id, s.label, s.user_agent, s.created_at, s.last_used_at, s.expires_at, s.revoked_at";
const TOKEN_COLS: &str =
    "t.id, t.user_id, t.name, t.hint, t.created_at, t.last_used_at, t.expires_at, t.revoked_at";
pub struct Auth<'a> {
    pub(crate) pool: &'a PgPool,
}

impl Auth<'_> {
    /// Starts a device session: its token, shown once, and the session.
    pub async fn create_session(
        &self,
        user: Uuid,
        label: Option<&str>,
        user_agent: Option<&str>,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<(String, DeviceSession)> {
        let token = random_token(SESSION_PREFIX);
        let session = sqlx::query_as(AssertSqlSafe(format!(
            "INSERT INTO rookery.device_sessions AS s (id, user_id, token_hash, label, user_agent, expires_at, last_used_at)
             VALUES ($1, $2, $3, $4, $5, $6, now()) RETURNING {SESSION_COLS}"
        )))
        .bind(new_id())
        .bind(user)
        .bind(hash_token(&token))
        .bind(label)
        .bind(user_agent)
        .bind(expires_at)
        .fetch_one(self.pool)
        .await?;
        Ok((token, session))
    }

    /// The session and user of a token that still works: not revoked, not expired, the user not disabled.
    pub async fn resolve_session(&self, token: &str) -> Result<Option<(DeviceSession, User)>> {
        let found: Option<DeviceSession> = sqlx::query_as(AssertSqlSafe(format!(
            "UPDATE rookery.device_sessions s SET last_used_at = now()
             FROM rookery.users u
             WHERE s.token_hash = $1 AND u.id = s.user_id AND s.revoked_at IS NULL
               AND (s.expires_at IS NULL OR s.expires_at > now()) AND u.disabled_at IS NULL
             RETURNING {SESSION_COLS}"
        )))
        .bind(hash_token(token))
        .fetch_optional(self.pool)
        .await?;
        let Some(session) = found else {
            return Ok(None);
        };
        let user = Users { pool: self.pool }.get(session.user_id).await?;
        Ok(user.map(|user| (session, user)))
    }

    /// The sessions of a user that still work, newest first.
    pub async fn list_sessions(&self, user: Uuid) -> Result<Vec<DeviceSession>> {
        Ok(sqlx::query_as(AssertSqlSafe(format!(
            "SELECT {SESSION_COLS} FROM rookery.device_sessions s
             WHERE s.user_id = $1 AND s.revoked_at IS NULL AND (s.expires_at IS NULL OR s.expires_at > now())
             ORDER BY s.created_at DESC"
        )))
        .bind(user)
        .fetch_all(self.pool)
        .await?)
    }

    /// Ends one of a user's sessions.
    pub async fn revoke_session(&self, user: Uuid, id: Uuid) -> Result<()> {
        let done = sqlx::query(
            "UPDATE rookery.device_sessions SET revoked_at = coalesce(revoked_at, now()) WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user)
        .execute(self.pool)
        .await?;
        if done.rows_affected() == 0 {
            return Err(Error::NotFound("no such device".into()));
        }
        Ok(())
    }

    /// Ends the session of a token, such as on signing out. Unknown tokens are ignored.
    pub async fn revoke_session_token(&self, token: &str) -> Result<Option<Uuid>> {
        Ok(sqlx::query_scalar(
            "UPDATE rookery.device_sessions SET revoked_at = coalesce(revoked_at, now()) WHERE token_hash = $1 RETURNING id",
        )
        .bind(hash_token(token))
        .fetch_optional(self.pool)
        .await?)
    }

    /// Makes an access token: the token, shown once, and its record.
    pub async fn create_token(
        &self,
        user: Uuid,
        name: &str,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<(String, AccessToken)> {
        let token = random_token(TOKEN_PREFIX);
        let record = sqlx::query_as(AssertSqlSafe(format!(
            "INSERT INTO rookery.access_tokens AS t (id, user_id, name, token_hash, hint, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING {TOKEN_COLS}"
        )))
        .bind(new_id())
        .bind(user)
        .bind(name)
        .bind(hash_token(&token))
        .bind(hint(&token))
        .bind(expires_at)
        .fetch_one(self.pool)
        .await?;
        Ok((token, record))
    }

    /// The token and user of an access token that still works.
    pub async fn resolve_token(&self, token: &str) -> Result<Option<(AccessToken, User)>> {
        let found: Option<AccessToken> = sqlx::query_as(AssertSqlSafe(format!(
            "UPDATE rookery.access_tokens t SET last_used_at = now()
             FROM rookery.users u
             WHERE t.token_hash = $1 AND u.id = t.user_id AND t.revoked_at IS NULL
               AND (t.expires_at IS NULL OR t.expires_at > now()) AND u.disabled_at IS NULL
             RETURNING {TOKEN_COLS}"
        )))
        .bind(hash_token(token))
        .fetch_optional(self.pool)
        .await?;
        let Some(record) = found else { return Ok(None) };
        let user = Users { pool: self.pool }.get(record.user_id).await?;
        Ok(user.map(|user| (record, user)))
    }

    pub async fn list_tokens(&self, user: Uuid) -> Result<Vec<AccessToken>> {
        Ok(sqlx::query_as(AssertSqlSafe(format!(
            "SELECT {TOKEN_COLS} FROM rookery.access_tokens t
             WHERE t.user_id = $1 AND t.revoked_at IS NULL AND (t.expires_at IS NULL OR t.expires_at > now())
             ORDER BY t.created_at DESC"
        )))
        .bind(user)
        .fetch_all(self.pool)
        .await?)
    }

    pub async fn revoke_token(&self, user: Uuid, id: Uuid) -> Result<()> {
        let done = sqlx::query(
            "UPDATE rookery.access_tokens SET revoked_at = coalesce(revoked_at, now()) WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user)
        .execute(self.pool)
        .await?;
        if done.rows_affected() == 0 {
            return Err(Error::NotFound("no such token".into()));
        }
        Ok(())
    }
}
