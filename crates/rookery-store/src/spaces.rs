//! Spaces. A space is a scope that data, settings, storage and connections belong to; its entity is
//! `space://<id>`. Membership is a grant on that URI, so that what is in a space inherits its members'
//! levels. Every person has a private personal space.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Store;
use crate::access::Grant;
use crate::caller::{Caller, Level};
use crate::entities::EntityRecord;
use crate::error::{Error, Result};
use crate::uri::space_uri;
use crate::users::User;
use crate::util::{new_id, slugify};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Space {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
    pub slug: String,
    pub owner_id: Option<Uuid>,
    pub personal: bool,
    pub data: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
}

impl Space {
    pub fn uri(&self) -> String {
        space_uri(self.id)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSpace {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub data: Option<Value>,
}

pub struct Spaces<'a> {
    pub(crate) store: &'a Store,
}

impl<'a> Spaces<'a> {
    /// Creates a space owned by the caller's user.
    pub async fn create(&self, caller: &Caller, new: &NewSpace) -> Result<Space> {
        let owner = caller.require_user()?;
        if new.kind.is_empty() || new.kind == "personal" {
            return Err(Error::invalid("a space needs a kind other than personal"));
        }
        let mut tx = self.store.pool().begin().await?;
        let slug = free_slug(&mut tx, "", new.slug.as_deref().unwrap_or(&new.name)).await?;
        let s: Space = sqlx::query_as(
            "INSERT INTO rookery.spaces (id, kind, name, slug, owner_id, data) VALUES ($1, $2, $3, $4, $5, $6) RETURNING *",
        )
        .bind(new_id())
        .bind(&new.kind)
        .bind(&new.name)
        .bind(&slug)
        .bind(owner)
        .bind(new.data.clone().unwrap_or_else(|| Value::Object(Default::default())))
        .fetch_one(&mut *tx)
        .await?;
        index_space(&mut tx, &s).await?;
        tx.commit().await?;
        Ok(s)
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<Space>> {
        Ok(sqlx::query_as("SELECT * FROM rookery.spaces WHERE id = $1")
            .bind(id)
            .fetch_optional(self.store.pool())
            .await?)
    }

    pub async fn by_slug(&self, slug: &str) -> Result<Option<Space>> {
        Ok(
            sqlx::query_as("SELECT * FROM rookery.spaces WHERE slug = $1")
                .bind(slug)
                .fetch_optional(self.store.pool())
                .await?,
        )
    }

    /// The user's personal space, created if missing.
    pub async fn personal(&self, user: Uuid) -> Result<Space> {
        let mut tx = self.store.pool().begin().await?;
        let u: User = sqlx::query_as("SELECT * FROM rookery.users WHERE id = $1")
            .bind(user)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| Error::not_found("no such user"))?;
        let s = ensure_personal(&mut tx, &u).await?;
        tx.commit().await?;
        Ok(s)
    }

    /// Spaces the caller may view, excluding other users' personal spaces and archived ones.
    pub async fn list_for(&self, caller: &Caller) -> Result<Vec<Space>> {
        Ok(sqlx::query_as(
            "SELECT s.* FROM rookery.spaces s \
             WHERE s.archived_at IS NULL AND (NOT s.personal OR s.owner_id = $1) \
               AND ($3 OR rookery.effective_level('space://' || s.id::text, $1, rookery.principals_of($1, $2)) >= 1) \
             ORDER BY s.personal DESC, s.name",
        )
        .bind(caller.user)
        .bind(caller.actor())
        .bind(caller.admin)
        .fetch_all(self.store.pool())
        .await?)
    }

    /// Adds a member (a user or a group) at a level, or changes their level. Needs `manage`.
    pub async fn add_member(
        &self,
        caller: &Caller,
        space: Uuid,
        member: &str,
        level: Level,
    ) -> Result<Grant> {
        let s = self
            .get(space)
            .await?
            .ok_or_else(|| Error::not_found("no such space"))?;
        if s.personal {
            return Err(Error::invalid("a personal space has no members"));
        }
        let uri = space_uri(space);
        self.store
            .access()
            .require(caller, &uri, Level::Manage)
            .await?;
        let grant = self
            .store
            .access()
            .grant(&uri, member, level.as_i16(), &caller.actor())
            .await?;
        self.store
            .audit()
            .record(
                &caller.actor(),
                "spaces.addMember",
                Some(&uri),
                serde_json::json!({ "principal": grant.principal, "level": level }),
            )
            .await?;
        Ok(grant)
    }

    pub async fn remove_member(&self, caller: &Caller, space: Uuid, member: &str) -> Result<bool> {
        let uri = space_uri(space);
        self.store
            .access()
            .require(caller, &uri, Level::Manage)
            .await?;
        let removed = self.store.access().revoke(&uri, member).await?;
        if removed {
            self.store
                .audit()
                .record(
                    &caller.actor(),
                    "spaces.removeMember",
                    Some(&uri),
                    serde_json::json!({ "principal": member }),
                )
                .await?;
        }
        Ok(removed)
    }

    /// The members: the grants on the space. The owner is not listed; it holds manage.
    pub async fn members(&self, space: Uuid) -> Result<Vec<Grant>> {
        self.store.access().grants(&space_uri(space)).await
    }

    /// Renames a space. Needs `manage`.
    pub async fn rename(&self, caller: &Caller, space: Uuid, name: &str) -> Result<Space> {
        let mut tx = self.store.pool().begin().await?;
        crate::access::require(&mut tx, caller, &space_uri(space), Level::Manage).await?;
        let s: Space = sqlx::query_as(
            "UPDATE rookery.spaces SET name = $2, updated_at = now() WHERE id = $1 RETURNING *",
        )
        .bind(space)
        .bind(name)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::not_found("no such space"))?;
        index_space(&mut tx, &s).await?;
        tx.commit().await?;
        Ok(s)
    }

    /// Archives (or unarchives) a space. Needs `manage`.
    pub async fn set_archived(&self, caller: &Caller, space: Uuid, archived: bool) -> Result<()> {
        let mut tx = self.store.pool().begin().await?;
        crate::access::require(&mut tx, caller, &space_uri(space), Level::Manage).await?;
        sqlx::query(
            "UPDATE rookery.spaces SET archived_at = CASE WHEN $2 THEN now() ELSE NULL END, updated_at = now() WHERE id = $1",
        )
        .bind(space)
        .bind(archived)
        .execute(&mut *tx)
        .await?;
        crate::audit::record_on(
            &mut tx,
            &caller.actor(),
            "spaces.archive",
            Some(&space_uri(space)),
            serde_json::json!({ "archived": archived }),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

async fn free_slug(conn: &mut PgConnection, prefix: &str, wanted: &str) -> Result<String> {
    let base = format!("{prefix}{}", slugify(wanted));
    let mut slug = base.clone();
    for n in 2.. {
        let taken: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM rookery.spaces WHERE slug = $1)")
                .bind(&slug)
                .fetch_one(&mut *conn)
                .await?;
        if !taken {
            break;
        }
        slug = format!("{base}-{n}");
    }
    Ok(slug)
}

async fn index_space(conn: &mut PgConnection, s: &Space) -> Result<()> {
    crate::entities::upsert(
        conn,
        &EntityRecord {
            uri: s.uri(),
            space_id: Some(s.id),
            title: s.name.clone(),
            subtitle: Some(s.kind.clone()),
            owner_id: s.owner_id,
            private: s.personal,
            ..Default::default()
        },
        None,
    )
    .await
}

pub(crate) async fn ensure_personal(conn: &mut PgConnection, user: &User) -> Result<Space> {
    if let Some(s) =
        sqlx::query_as::<_, Space>("SELECT * FROM rookery.spaces WHERE owner_id = $1 AND personal")
            .bind(user.id)
            .fetch_optional(&mut *conn)
            .await?
    {
        return Ok(s);
    }
    let local = user
        .email
        .as_deref()
        .and_then(|e| e.split('@').next())
        .unwrap_or(&user.name);
    let slug = free_slug(conn, "~", local).await?;
    let s: Space = sqlx::query_as(
        "INSERT INTO rookery.spaces (id, kind, name, slug, owner_id, personal) VALUES ($1, 'personal', $2, $3, $4, true) \
         RETURNING *",
    )
    .bind(new_id())
    .bind(if user.name.is_empty() { "Personal" } else { &user.name })
    .bind(slug)
    .bind(user.id)
    .fetch_one(&mut *conn)
    .await?;
    index_space(conn, &s).await?;
    Ok(s)
}
