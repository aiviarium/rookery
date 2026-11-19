//! Grants on resources, owners and groups.
//!
//! A caller's level on a resource is the highest of the grants to its principals (itself, its user,
//! the user's groups and `everyone`), and manage for the resource's owner. Levels: 1 view, 2 use,
//! 3 manage.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::util::new_id;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Grant {
    pub resource: String,
    pub principal: String,
    pub level: i16,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    pub claim: Option<String>,
    pub members: i64,
}

/// Who asks: the principal and, for a person, their user.
#[derive(Debug, Clone)]
pub struct Who<'a> {
    pub principal: &'a str,
    pub user: Option<Uuid>,
}

pub struct Access<'a> {
    pub(crate) pool: &'a PgPool,
}

/// A principal a grant may name: `user:<uuid>`, `group:<uuid>` or `everyone`.
pub fn check_principal(principal: &str) -> Result<()> {
    principal_name(principal).map(|_| ())
}

/// A principal as grants keep it, with its id in the one spelling the callers' principals have, so that
/// `user:<UUID>` in capitals or without hyphens names the same person.
pub fn principal_name(principal: &str) -> Result<String> {
    if principal == "everyone" {
        return Ok(principal.to_owned());
    }
    let named = ["user:", "group:"].iter().find_map(|kind| {
        let id = principal.strip_prefix(kind)?;
        Uuid::parse_str(id).ok().map(|id| format!("{kind}{id}"))
    });
    named.ok_or_else(|| {
        Error::Invalid(format!(
            "{principal:?} is not user:<id>, group:<id> or everyone"
        ))
    })
}

/// A resource URI such as `env://studio`.
pub fn check_resource(resource: &str) -> Result<()> {
    let ok = resource.len() <= 500
        && resource.split_once("://").is_some_and(|(scheme, rest)| {
            !scheme.is_empty()
                && scheme
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && !rest.is_empty()
        });
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "{resource:?} is not a resource such as env://NAME"
        )))
    }
}

const PRINCIPALS: &str = "SELECT unnest(ARRAY[$2::text, 'everyone'] || CASE WHEN $3::uuid IS NULL THEN '{}'::text[] ELSE ARRAY['user:' || $3::text] END
    || coalesce((SELECT array_agg('group:' || m.group_id::text) FROM rookery.group_members m WHERE m.user_id = $3), '{}'::text[]))";

impl Access<'_> {
    pub async fn grant(
        &self,
        resource: &str,
        principal: &str,
        level: i16,
        by: &str,
    ) -> Result<Grant> {
        check_resource(resource)?;
        let principal = &principal_name(principal)?;
        if !(1..=3).contains(&level) {
            return Err(Error::Invalid("a level is view, use or manage".into()));
        }
        Ok(sqlx::query_as(
            "INSERT INTO rookery.grants (resource, principal, level, created_by) VALUES ($1, $2, $3, $4)
             ON CONFLICT (resource, principal) DO UPDATE SET level = EXCLUDED.level, created_by = EXCLUDED.created_by, created_at = now()
             RETURNING resource, principal, level, created_by, created_at",
        )
        .bind(resource)
        .bind(principal)
        .bind(level)
        .bind(by)
        .fetch_one(self.pool)
        .await?)
    }

    pub async fn revoke(&self, resource: &str, principal: &str) -> Result<bool> {
        let principal = &principal_name(principal)?;
        let done = sqlx::query("DELETE FROM rookery.grants WHERE resource = $1 AND principal = $2")
            .bind(resource)
            .bind(principal)
            .execute(self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn grants(&self, resource: &str) -> Result<Vec<Grant>> {
        Ok(sqlx::query_as(
            "SELECT resource, principal, level, created_by, created_at FROM rookery.grants WHERE resource = $1 ORDER BY level DESC, principal",
        )
        .bind(resource)
        .fetch_all(self.pool)
        .await?)
    }

    /// Removes every grant and the owner of a resource, such as when it is removed.
    pub async fn forget(&self, resource: &str) -> Result<()> {
        sqlx::query("DELETE FROM rookery.grants WHERE resource = $1")
            .bind(resource)
            .execute(self.pool)
            .await?;
        sqlx::query("DELETE FROM rookery.owners WHERE resource = $1")
            .bind(resource)
            .execute(self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_owner(&self, resource: &str, user: Uuid) -> Result<()> {
        check_resource(resource)?;
        sqlx::query("INSERT INTO rookery.owners (resource, user_id) VALUES ($1, $2) ON CONFLICT (resource) DO UPDATE SET user_id = EXCLUDED.user_id")
            .bind(resource)
            .bind(user)
            .execute(self.pool)
            .await?;
        Ok(())
    }

    pub async fn owner(&self, resource: &str) -> Result<Option<Uuid>> {
        Ok(
            sqlx::query_scalar("SELECT user_id FROM rookery.owners WHERE resource = $1")
                .bind(resource)
                .fetch_optional(self.pool)
                .await?,
        )
    }

    /// The level `who` holds on a resource: 0 for none.
    pub async fn level(&self, resource: &str, who: &Who<'_>) -> Result<i16> {
        let level: Option<i16> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT greatest(
                 (SELECT max(g.level) FROM rookery.grants g WHERE g.resource = $1 AND g.principal IN ({PRINCIPALS})),
                 (SELECT 3::smallint FROM rookery.owners o WHERE o.resource = $1 AND o.user_id = $3))"
        )))
        .bind(resource)
        .bind(who.principal)
        .bind(who.user)
        .fetch_one(self.pool)
        .await?;
        Ok(level.unwrap_or(0))
    }

    /// Those of `resources` on which `who` holds at least `level`.
    pub async fn filter(
        &self,
        resources: &[String],
        who: &Who<'_>,
        level: i16,
    ) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT r FROM unnest($1::text[]) AS r WHERE greatest(
                 (SELECT max(g.level) FROM rookery.grants g WHERE g.resource = r AND g.principal IN ({PRINCIPALS_4})),
                 (SELECT 3::smallint FROM rookery.owners o WHERE o.resource = r AND o.user_id = $3)) >= $4",
            PRINCIPALS_4 = PRINCIPALS
        )))
        .bind(resources)
        .bind(who.principal)
        .bind(who.user)
        .bind(level)
        .fetch_all(self.pool)
        .await?)
    }

    // ── groups ──────────────────────────────────────────────────────────────────────────────────

    pub async fn create_group(&self, name: &str, claim: Option<&str>) -> Result<Group> {
        let made = sqlx::query_as(
            "INSERT INTO rookery.groups (id, name, claim) VALUES ($1, $2, $3) RETURNING id, name, claim, 0::bigint AS members",
        )
        .bind(new_id())
        .bind(name)
        .bind(claim)
        .fetch_one(self.pool)
        .await
        .map_err(Error::from);
        match made {
            Err(e) if e.is_unique_violation() => Err(Error::Conflict(format!(
                "there is a group {name} or one with that claim"
            ))),
            other => other,
        }
    }

    pub async fn delete_group(&self, id: Uuid) -> Result<()> {
        let done = sqlx::query("DELETE FROM rookery.groups WHERE id = $1")
            .bind(id)
            .execute(self.pool)
            .await?;
        if done.rows_affected() == 0 {
            return Err(Error::NotFound("no such group".into()));
        }
        sqlx::query("DELETE FROM rookery.grants WHERE principal = $1")
            .bind(format!("group:{id}"))
            .execute(self.pool)
            .await?;
        Ok(())
    }

    pub async fn groups(&self) -> Result<Vec<Group>> {
        Ok(sqlx::query_as(
            "SELECT g.id, g.name, g.claim, count(m.user_id) AS members FROM rookery.groups g
             LEFT JOIN rookery.group_members m ON m.group_id = g.id GROUP BY g.id ORDER BY lower(g.name)",
        )
        .fetch_all(self.pool)
        .await?)
    }

    pub async fn members(&self, group: Uuid) -> Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar(
            "SELECT user_id FROM rookery.group_members WHERE group_id = $1 ORDER BY user_id",
        )
        .bind(group)
        .fetch_all(self.pool)
        .await?)
    }

    pub async fn add_member(&self, group: Uuid, user: Uuid) -> Result<()> {
        let done = sqlx::query("INSERT INTO rookery.group_members (group_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(group)
            .bind(user)
            .execute(self.pool)
            .await;
        match done {
            Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23503") => {
                Err(Error::NotFound("no such group or user".into()))
            }
            other => other.map(|_| ()).map_err(Error::from),
        }
    }

    pub async fn remove_member(&self, group: Uuid, user: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM rookery.group_members WHERE group_id = $1 AND user_id = $2")
            .bind(group)
            .bind(user)
            .execute(self.pool)
            .await?;
        Ok(())
    }

    /// Makes a person a member of exactly those groups with a claim that their identity provider names.
    pub async fn sync_claims(&self, user: Uuid, claims: &[String]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM rookery.group_members m USING rookery.groups g
             WHERE m.group_id = g.id AND m.user_id = $1 AND g.claim IS NOT NULL AND NOT (g.claim = ANY ($2))",
        )
        .bind(user)
        .bind(claims)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO rookery.group_members (group_id, user_id)
             SELECT id, $1 FROM rookery.groups WHERE claim = ANY ($2) ON CONFLICT DO NOTHING",
        )
        .bind(user)
        .bind(claims)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
