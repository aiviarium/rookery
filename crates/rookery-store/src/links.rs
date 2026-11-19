//! One-time sign-in links for local accounts. A link lasts a day and works once; the store keeps a hash
//! of its code.

use chrono::{DateTime, Duration, Utc};
use rookery_proto::auth::LinkKind;
use sqlx::AssertSqlSafe;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::util::{hash_token, new_id, random_token};

/// How long a link works.
pub const LIFETIME_HOURS: i64 = 24;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Link {
    pub id: Uuid,
    pub kind: String,
    pub user_id: Option<Uuid>,
    pub name: Option<String>,
    pub email: Option<String>,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl Link {
    pub fn kind(&self) -> LinkKind {
        match self.kind.as_str() {
            "setup" => LinkKind::Setup,
            "invite" => LinkKind::Invite,
            _ => LinkKind::Device,
        }
    }
}

/// A link that can still be used, with the names that go with it.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Pending {
    #[sqlx(flatten)]
    pub link: Link,
    pub user_name: Option<String>,
    pub created_by_name: Option<String>,
}

pub fn kind_name(kind: LinkKind) -> &'static str {
    match kind {
        LinkKind::Setup => "setup",
        LinkKind::Invite => "invite",
        LinkKind::Device => "device",
    }
}

const COLS: &str = "l.id, l.kind, l.user_id, l.name, l.email, l.created_by, l.created_at, l.expires_at, l.used_at, l.revoked_at";
const OPEN: &str = "l.used_at IS NULL AND l.revoked_at IS NULL AND l.expires_at > now()";

pub struct Links<'a> {
    pub(crate) pool: &'a PgPool,
}

impl Links<'_> {
    /// Makes a link: its code, shown once, and its record.
    pub async fn create(
        &self,
        kind: LinkKind,
        user: Option<Uuid>,
        name: Option<&str>,
        email: Option<&str>,
        created_by: Option<Uuid>,
    ) -> Result<(String, Link)> {
        if (kind == LinkKind::Device) != user.is_some() {
            return Err(Error::Invalid(
                "a device link names its user, and only a device link does".into(),
            ));
        }
        let code = random_token("");
        let link = sqlx::query_as(AssertSqlSafe(format!(
            "INSERT INTO rookery.sign_in_links AS l (id, code_hash, kind, user_id, name, email, created_by, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {COLS}"
        )))
        .bind(new_id())
        .bind(hash_token(&code))
        .bind(kind_name(kind))
        .bind(user)
        .bind(name)
        .bind(email)
        .bind(created_by)
        .bind(Utc::now() + Duration::hours(LIFETIME_HOURS))
        .fetch_one(self.pool)
        .await?;
        Ok((code, link))
    }

    /// The link of a code that still works, without using it.
    pub async fn peek(&self, code: &str) -> Result<Option<Link>> {
        Ok(sqlx::query_as(AssertSqlSafe(format!(
            "SELECT {COLS} FROM rookery.sign_in_links l WHERE l.code_hash = $1 AND {OPEN}"
        )))
        .bind(hash_token(code))
        .fetch_optional(self.pool)
        .await?)
    }

    /// Uses the link of a code. Only one caller gets it.
    pub async fn consume(&self, code: &str) -> Result<Option<Link>> {
        Ok(sqlx::query_as(AssertSqlSafe(format!(
            "UPDATE rookery.sign_in_links l SET used_at = now() WHERE l.code_hash = $1 AND {OPEN} RETURNING {COLS}"
        )))
        .bind(hash_token(code))
        .fetch_optional(self.pool)
        .await?)
    }

    /// The links that still work: all of them, or those `by` made.
    pub async fn pending(&self, by: Option<Uuid>) -> Result<Vec<Pending>> {
        Ok(sqlx::query_as(AssertSqlSafe(format!(
            "SELECT {COLS}, u.name AS user_name, c.name AS created_by_name
             FROM rookery.sign_in_links l
             LEFT JOIN rookery.users u ON u.id = l.user_id
             LEFT JOIN rookery.users c ON c.id = l.created_by
             WHERE {OPEN} AND ($1::uuid IS NULL OR l.created_by = $1)
             ORDER BY l.created_at DESC"
        )))
        .bind(by)
        .fetch_all(self.pool)
        .await?)
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<Link>> {
        Ok(sqlx::query_as(AssertSqlSafe(format!(
            "SELECT {COLS} FROM rookery.sign_in_links l WHERE l.id = $1"
        )))
        .bind(id)
        .fetch_optional(self.pool)
        .await?)
    }

    /// Ends a link that still works.
    pub async fn revoke(&self, id: Uuid) -> Result<Link> {
        sqlx::query_as(AssertSqlSafe(format!("UPDATE rookery.sign_in_links l SET revoked_at = now() WHERE l.id = $1 AND {OPEN} RETURNING {COLS}")))
            .bind(id)
            .fetch_optional(self.pool)
            .await?
            .ok_or_else(|| Error::NotFound("no such link, or it was used or has ended".into()))
    }
}
