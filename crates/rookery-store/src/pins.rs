//! Pins: entities and views a user keeps in the sidebar and on the home screen, and the ones a space
//! shares with its members. Personal pins follow the user to every device; shared pins belong to the
//! space and show for everyone who can view it. A pinned entity is listed only while the caller may
//! view it; a pinned view only while they may view the space it opens in.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Store;
use crate::caller::{Caller, Level};
use crate::entities::{Entity, visible_sql};
use crate::error::{Error, Result};
use crate::util::{new_id, truncate_str};

/// At most this many personal pins per user, and shared pins per space.
pub const MAX_PINS: i64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PinKind {
    Entity,
    View,
}

impl PinKind {
    fn as_str(self) -> &'static str {
        match self {
            PinKind::Entity => "entity",
            PinKind::View => "view",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewPin {
    pub kind: PinKind,
    /// An entity's URI, or a view's id.
    pub target: String,
    #[serde(default)]
    pub path: String,
    /// The space a view opens in.
    #[serde(default)]
    pub view_space: Option<Uuid>,
    /// Shared with this space's members instead of personal. Needs `use` on the space.
    #[serde(default)]
    pub shared_in: Option<Uuid>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pin {
    pub id: Uuid,
    /// The space it is shared in, or `None` for a personal pin.
    pub space_id: Option<Uuid>,
    pub kind: PinKind,
    pub target: String,
    pub path: String,
    pub view_space: Option<Uuid>,
    pub title: String,
    pub icon: Option<String>,
    pub position: i64,
    pub created_at: DateTime<Utc>,
    /// The entity, for a pinned entity, with its current title and icon.
    pub entity: Option<Entity>,
}

pub struct Pins<'a> {
    pub(crate) store: &'a Store,
}

impl<'a> Pins<'a> {
    /// The caller's personal pins and the shared pins of `space`, or of every space they can view when
    /// none is named. Personal pins first, each group in the order it was pinned.
    pub async fn list(&self, caller: &Caller, space: Option<Uuid>) -> Result<Vec<Pin>> {
        let user = caller.require_user()?;
        if let Some(s) = space {
            self.store
                .access()
                .require(caller, &format!("space://{s}"), Level::View)
                .await?;
        }
        let sql = format!(
            "SELECT p.id, p.space_id, p.kind, p.target, p.path, p.view_space, p.title, p.icon, p.position, \
                    p.created_at, {ENTITY_COLUMNS_OPT} \
             FROM rookery.pins p LEFT JOIN rookery.entities e ON p.kind = 'entity' AND e.uri = p.target \
             WHERE (p.user_id = $1 OR (p.space_id IS NOT NULL AND ($4::uuid IS NULL OR p.space_id = $4) \
                    AND ($3 OR rookery.effective_level('space://' || p.space_id::text, $1, rookery.principals_of($1, $2)) >= 1))) \
               AND (p.kind = 'view' \
                    OR (e.uri IS NOT NULL AND e.deleted_at IS NULL AND {})) \
               AND (p.view_space IS NULL \
                    OR $3 OR rookery.effective_level('space://' || p.view_space::text, $1, rookery.principals_of($1, $2)) >= 1) \
             ORDER BY (p.user_id IS NULL), p.position, p.created_at",
            visible_sql("$1", "$2", "$3"),
            ENTITY_COLUMNS_OPT = ENTITY_COLUMNS_OPT,
        );
        let rows: Vec<PinRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql.to_string()))
            .bind(user)
            .bind(caller.actor())
            .bind(caller.admin)
            .bind(space)
            .fetch_all(self.store.pool())
            .await?;
        Ok(rows.into_iter().map(PinRow::into_pin).collect())
    }

    /// Pins something. Pinning the same thing twice returns the first pin.
    pub async fn add(&self, caller: &Caller, new: &NewPin) -> Result<Pin> {
        let user = caller.require_user()?;
        let (target, path) = match new.kind {
            PinKind::Entity => {
                crate::uri::Uri::parse(&new.target)?;
                if self
                    .store
                    .entities()
                    .get(caller, &new.target)
                    .await?
                    .is_none()
                {
                    return Err(Error::not_found(format!(
                        "{} does not exist, or it is not shared with you",
                        new.target
                    )));
                }
                (new.target.clone(), String::new())
            }
            PinKind::View => {
                if new.target.is_empty() || new.target.len() > 200 {
                    return Err(Error::invalid("a pinned view needs its id"));
                }
                if let Some(s) = new.view_space {
                    self.store
                        .access()
                        .require(caller, &format!("space://{s}"), Level::View)
                        .await?;
                }
                (new.target.clone(), truncate_str(&new.path, 500).to_string())
            }
        };
        let owner = match new.shared_in {
            Some(space) => {
                self.store
                    .access()
                    .require(caller, &format!("space://{space}"), Level::Use)
                    .await?;
                (None, Some(space))
            }
            None => (Some(user), None),
        };
        let title = truncate_str(&new.title, 200);
        let mut tx = self.store.pool().begin().await?;
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM rookery.pins WHERE kind = $1 AND target = $2 AND path = $3 \
               AND user_id IS NOT DISTINCT FROM $4 AND space_id IS NOT DISTINCT FROM $5",
        )
        .bind(new.kind.as_str())
        .bind(&target)
        .bind(&path)
        .bind(owner.0)
        .bind(owner.1)
        .fetch_optional(&mut *tx)
        .await?;
        let id = match existing {
            Some(id) => id,
            None => {
                let count: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM rookery.pins WHERE user_id IS NOT DISTINCT FROM $1 \
                       AND space_id IS NOT DISTINCT FROM $2",
                )
                .bind(owner.0)
                .bind(owner.1)
                .fetch_one(&mut *tx)
                .await?;
                if count >= MAX_PINS {
                    return Err(Error::invalid(format!(
                        "at most {MAX_PINS} pins; remove one first"
                    )));
                }
                let id = new_id();
                sqlx::query(
                    "INSERT INTO rookery.pins (id, user_id, space_id, created_by, kind, target, path, view_space, \
                                            title, icon, position) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
                             (SELECT coalesce(max(position), 0) + 1 FROM rookery.pins \
                              WHERE user_id IS NOT DISTINCT FROM $2 AND space_id IS NOT DISTINCT FROM $3))",
                )
                .bind(id)
                .bind(owner.0)
                .bind(owner.1)
                .bind(user)
                .bind(new.kind.as_str())
                .bind(&target)
                .bind(&path)
                .bind(new.view_space)
                .bind(title)
                .bind(&new.icon)
                .execute(&mut *tx)
                .await?;
                id
            }
        };
        tx.commit().await?;
        let all = self.list(caller, owner.1).await?;
        all.into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| Error::internal("a pin vanished"))
    }

    /// Removes a pin: a personal one by its owner, a shared one by someone with `use` on its space.
    /// Returns whether there was one to remove.
    pub async fn remove(&self, caller: &Caller, id: Uuid) -> Result<Option<Pin>> {
        let user = caller.require_user()?;
        let row: Option<(Option<Uuid>, Option<Uuid>)> =
            sqlx::query_as("SELECT user_id, space_id FROM rookery.pins WHERE id = $1")
                .bind(id)
                .fetch_optional(self.store.pool())
                .await?;
        let Some((owner, space)) = row else {
            return Ok(None);
        };
        if owner.is_some_and(|o| o != user) {
            // Somebody else's personal pin does not exist for this caller.
            return Ok(None);
        }
        if let Some(space) = space {
            self.store
                .access()
                .require(caller, &format!("space://{space}"), Level::Use)
                .await?;
        }
        let removed = self
            .list(caller, space)
            .await?
            .into_iter()
            .find(|p| p.id == id);
        sqlx::query("DELETE FROM rookery.pins WHERE id = $1")
            .bind(id)
            .execute(self.store.pool())
            .await?;
        Ok(removed)
    }
}

const ENTITY_COLUMNS_OPT: &str = "e.uri AS e_uri, e.scheme AS e_scheme, e.id AS e_id, e.space_id AS e_space_id, \
    e.parent AS e_parent, e.title AS e_title, e.subtitle AS e_subtitle, e.icon AS e_icon, \
    e.owner_id AS e_owner_id, e.private AS e_private, e.plugin AS e_plugin, e.collection AS e_collection, \
    e.created_at AS e_created_at, e.updated_at AS e_updated_at, e.deleted_at AS e_deleted_at";

#[derive(sqlx::FromRow)]
struct PinRow {
    id: Uuid,
    space_id: Option<Uuid>,
    kind: String,
    target: String,
    path: String,
    view_space: Option<Uuid>,
    title: String,
    icon: Option<String>,
    position: i64,
    created_at: DateTime<Utc>,
    e_uri: Option<String>,
    e_scheme: Option<String>,
    e_id: Option<String>,
    e_space_id: Option<Uuid>,
    e_parent: Option<String>,
    e_title: Option<String>,
    e_subtitle: Option<String>,
    e_icon: Option<String>,
    e_owner_id: Option<Uuid>,
    e_private: Option<bool>,
    e_plugin: Option<String>,
    e_collection: Option<String>,
    e_created_at: Option<DateTime<Utc>>,
    e_updated_at: Option<DateTime<Utc>>,
    e_deleted_at: Option<DateTime<Utc>>,
}

impl PinRow {
    fn into_pin(self) -> Pin {
        let entity = match (
            self.e_uri,
            self.e_scheme,
            self.e_id,
            self.e_created_at,
            self.e_updated_at,
        ) {
            (Some(uri), Some(scheme), Some(id), Some(created_at), Some(updated_at)) => {
                Some(Entity {
                    uri,
                    scheme,
                    id,
                    space_id: self.e_space_id,
                    parent: self.e_parent,
                    title: self.e_title.unwrap_or_default(),
                    subtitle: self.e_subtitle,
                    icon: self.e_icon,
                    owner_id: self.e_owner_id,
                    private: self.e_private.unwrap_or(false),
                    plugin: self.e_plugin,
                    collection: self.e_collection,
                    created_at,
                    updated_at,
                    deleted_at: self.e_deleted_at,
                })
            }
            _ => None,
        };
        Pin {
            id: self.id,
            space_id: self.space_id,
            kind: if self.kind == "entity" {
                PinKind::Entity
            } else {
                PinKind::View
            },
            target: self.target,
            path: self.path,
            view_space: self.view_space,
            title: self.title,
            icon: self.icon,
            position: self.position,
            created_at: self.created_at,
            entity,
        }
    }
}
