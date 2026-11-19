//! Grants on resources, owners and groups, and the check of a level.
//!
//! A caller's level on a resource is the highest of the grants to its principals (itself, its user,
//! the user's groups and `everyone`) on the resource or an ancestor, and manage for the owner of the
//! resource or of an ancestor. Ancestors are found by walking `parent` in the entity index, stopping
//! after a private entity, so a row of a space inherits the space's members. The walk is the SQL
//! function `rookery.effective_level`, which reads of collections filter rows with. Levels: 1 view,
//! 2 use, 3 manage. Admins hold manage on everything.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::caller::{Caller, Level};
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

/// A principal a grant may name: `user:<uuid>`, `group:<uuid>`, `plugin:<id>` or `everyone`.
pub fn check_principal(principal: &str) -> Result<()> {
    principal_name(principal).map(|_| ())
}

pub use crate::caller::principal_name;

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

    /// Takes every grant a principal holds away, and answers the resources it held one on, so that a
    /// plugin removed leaves nothing for a package that later takes its id.
    pub async fn forget_principal(&self, principal: &str) -> Result<Vec<String>> {
        let principal = &principal_name(principal)?;
        Ok(
            sqlx::query_scalar(
                "DELETE FROM rookery.grants WHERE principal = $1 RETURNING resource",
            )
            .bind(principal)
            .fetch_all(self.pool)
            .await?,
        )
    }

    /// Moves the grants and the owner of a resource to another, as when a host is renamed.
    pub async fn move_resource(&self, from: &str, to: &str) -> Result<()> {
        check_resource(to)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM rookery.grants WHERE resource = $1")
            .bind(to)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM rookery.owners WHERE resource = $1")
            .bind(to)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE rookery.grants SET resource = $2 WHERE resource = $1")
            .bind(from)
            .bind(to)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE rookery.owners SET resource = $2 WHERE resource = $1")
            .bind(from)
            .bind(to)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
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
        Ok(sqlx::query_scalar(
            "SELECT rookery.effective_level(coalesce((SELECT target FROM rookery.entity_aliases WHERE uri = $1), $1),
                 $3, rookery.principals_of($3, $2))",
        )
        .bind(resource)
        .bind(who.principal)
        .bind(who.user)
        .fetch_one(self.pool)
        .await?)
    }

    /// Those of `resources` on which `who` holds at least `level`, in their order.
    pub async fn filter(
        &self,
        resources: &[String],
        who: &Who<'_>,
        level: i16,
    ) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "WITH p AS (SELECT rookery.principals_of($3, $2) AS ps)
             SELECT r FROM unnest($1::text[]) WITH ORDINALITY AS t(r, n), p
             WHERE rookery.effective_level(r, $3, p.ps) >= $4 ORDER BY n",
        )
        .bind(resources)
        .bind(who.principal)
        .bind(who.user)
        .bind(level)
        .fetch_all(self.pool)
        .await?)
    }

    /// The caller's level on a resource, or `None` for none.
    pub async fn level_of(&self, caller: &Caller, resource: &str) -> Result<Option<Level>> {
        let mut conn = self.pool.acquire().await?;
        level(&mut conn, caller, resource).await
    }

    /// `FORBIDDEN` unless the caller holds at least `need` on a resource.
    pub async fn require(&self, caller: &Caller, resource: &str, need: Level) -> Result<()> {
        let mut conn = self.pool.acquire().await?;
        require(&mut conn, caller, resource, need).await
    }

    /// The principals the caller acts as.
    pub async fn principals(&self, caller: &Caller) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar("SELECT rookery.principals_of($1, $2)")
            .bind(caller.user)
            .bind(caller.actor())
            .fetch_one(self.pool)
            .await?)
    }

    // ── groups ──────────────────────────────────────────────────────────────────────────────────

    pub async fn create_group(&self, name: &str, claim: Option<&str>) -> Result<Group> {
        let mut tx = crate::actor::begin(self.pool).await?;
        let made: Result<Group> = sqlx::query_as(
            "INSERT INTO rookery.groups (id, name, claim) VALUES ($1, $2, $3) RETURNING id, name, claim, 0::bigint AS members",
        )
        .bind(new_id())
        .bind(name)
        .bind(claim)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::from);
        match made {
            Err(e) if e.is_unique_violation() => Err(Error::Conflict(format!(
                "there is a group {name} or one with that claim"
            ))),
            Err(e) => Err(e),
            Ok(group) => {
                tx.commit().await?;
                Ok(group)
            }
        }
    }

    pub async fn delete_group(&self, id: Uuid) -> Result<()> {
        let mut tx = crate::actor::begin(self.pool).await?;
        let done = sqlx::query("DELETE FROM rookery.groups WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        if done.rows_affected() == 0 {
            return Err(Error::NotFound("no such group".into()));
        }
        sqlx::query("DELETE FROM rookery.grants WHERE principal = $1")
            .bind(format!("group:{id}"))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
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

/// The caller's level on a resource. An alias has the access of the entity it names.
pub(crate) async fn level(
    conn: &mut sqlx::PgConnection,
    caller: &Caller,
    resource: &str,
) -> Result<Option<Level>> {
    if caller.admin {
        return Ok(Some(Level::Manage));
    }
    let l: i16 = sqlx::query_scalar(
        "SELECT rookery.effective_level(coalesce((SELECT target FROM rookery.entity_aliases WHERE uri = $1), $1),
             $2, rookery.principals_of($2, $3))",
    )
    .bind(resource)
    .bind(caller.user)
    .bind(caller.actor())
    .fetch_one(conn)
    .await?;
    Ok(Level::from_i16(l))
}

pub(crate) async fn require(
    conn: &mut sqlx::PgConnection,
    caller: &Caller,
    resource: &str,
    need: Level,
) -> Result<()> {
    match level(conn, caller, resource).await? {
        Some(l) if l >= need => Ok(()),
        _ => Err(Error::forbidden(format!("this needs {need} on {resource}"))),
    }
}
