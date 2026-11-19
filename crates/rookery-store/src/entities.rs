//! The entity index: everything with a URI, for global search, mention pickers, chips and
//! permission checks. Rows of entity collections are indexed by the store itself; other
//! entities (environments, terminals, agent sessions, anything a plugin registers) through
//! [`Entities::register`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Store;
use crate::caller::{Caller, Level};
use crate::error::Result;
use crate::uri::Uri;
use crate::util::truncate_str;

/// Text over this many bytes is cut before it goes into a `tsvector`.
pub(crate) const MAX_SEARCH_TEXT: usize = 256 * 1024;

/// An entity to register or update.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityRecord {
    pub uri: String,
    #[serde(default)]
    pub space_id: Option<Uuid>,
    /// Another entity's URI; permissions inherit along it.
    #[serde(default)]
    pub parent: Option<String>,
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    /// More text to search beside the title.
    #[serde(default)]
    pub search_text: Option<String>,
    /// The owner has `manage`.
    #[serde(default)]
    pub owner_id: Option<Uuid>,
    /// A private entity does not inherit grants from its parent.
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub plugin: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Entity {
    pub uri: String,
    pub scheme: String,
    pub id: String,
    pub space_id: Option<Uuid>,
    pub parent: Option<String>,
    pub title: String,
    pub subtitle: Option<String>,
    pub icon: Option<String>,
    pub owner_id: Option<Uuid>,
    pub private: bool,
    pub plugin: Option<String>,
    pub collection: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

/// `e` is visible (view) to the caller with user `$u` and principal string `$p`: either its
/// parent is a root the caller can see and it is not private, or the full walk says so.
pub(crate) fn visible_sql(u: &str, p: &str, admin: &str) -> String {
    format!(
        "({admin} OR (NOT e.private AND e.parent IN (SELECT rookery.visible_roots({u}, rookery.principals_of({u}, {p})))) \
          OR rookery.effective_level(e.uri, {u}, rookery.principals_of({u}, {p})) >= 1)"
    )
}

/// Like [`visible_sql`] for a scan over many entities, most of which the caller may not see: a row that
/// misses the fast path is only walked when it is among the entities reachable from the caller's grants
/// and ownership (`reach`, which the query must define with [`reach_cte`]; it is built once, and only when
/// a row needs it). Without that, every invisible match costs a walk up its parents.
pub(crate) fn visible_scan_sql(u: &str, p: &str, admin: &str) -> String {
    format!(
        "({admin} OR (NOT e.private AND e.parent IN (SELECT rookery.visible_roots({u}, rookery.principals_of({u}, {p})))) \
          OR (e.uri IN (SELECT uri FROM reach) \
              AND rookery.effective_level(e.uri, {u}, rookery.principals_of({u}, {p})) >= 1))"
    )
}

/// The `reach` CTE that [`visible_scan_sql`] reads. Empty for admins, who see everything.
pub(crate) fn reach_cte(u: &str, p: &str, admin: &str) -> String {
    format!(
        "reach AS MATERIALIZED (SELECT r AS uri FROM rookery.reachable({u}, rookery.principals_of({u}, {p})) r \
         WHERE NOT {admin})"
    )
}

pub(crate) const ENTITY_COLUMNS: &str = "e.uri, e.scheme, e.id, e.space_id, e.parent, e.title, e.subtitle, \
    e.icon, e.owner_id, e.private, e.plugin, e.collection, e.created_at, e.updated_at, e.deleted_at";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntitySearch {
    pub text: String,
    /// Only these schemes.
    #[serde(default)]
    pub schemes: Option<Vec<String>>,
    /// Only entities in this space.
    #[serde(default)]
    pub space: Option<Uuid>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityHit {
    #[serde(flatten)]
    pub entity: Entity,
    pub rank: f32,
}

pub struct Entities<'a> {
    pub(crate) store: &'a Store,
}

impl<'a> Entities<'a> {
    /// Registers an entity or updates it. Inbound references stop being dangling.
    pub async fn register(&self, rec: &EntityRecord) -> Result<Entity> {
        let mut tx = self.store.pool().begin().await?;
        upsert(&mut tx, rec, None).await?;
        let e = fetch(&mut tx, &rec.uri).await?.expect("just upserted");
        tx.commit().await?;
        Ok(e)
    }

    /// Removes an entity from the index. Inbound references stay and are marked dangling;
    /// grants on it are deleted.
    pub async fn remove(&self, uri: &str) -> Result<bool> {
        let mut tx = self.store.pool().begin().await?;
        let found = delete(&mut tx, uri).await?;
        tx.commit().await?;
        Ok(found)
    }

    /// The entity, if it exists and the caller may view it. Deleted (trashed) entities are
    /// returned too, with `deletedAt` set.
    pub async fn get(&self, caller: &Caller, uri: &str) -> Result<Option<Entity>> {
        Ok(self
            .get_many(caller, &[uri.to_string()])
            .await?
            .into_iter()
            .next())
    }

    /// The entities among `uris` that exist and the caller may view, in the given order. A URI that is an
    /// alias ([`Entities::alias`]) gives the entity it names.
    pub async fn get_many(&self, caller: &Caller, uris: &[String]) -> Result<Vec<Entity>> {
        let sql = format!(
            "SELECT {ENTITY_COLUMNS} FROM unnest($1::text[]) WITH ORDINALITY AS t(u, n) \
             LEFT JOIN rookery.entity_aliases a ON a.uri = t.u \
             JOIN rookery.entities e ON e.uri = COALESCE(a.target, t.u) \
             WHERE {} ORDER BY t.n",
            visible_sql("$2", "$3", "$4")
        );
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql.to_string()))
            .bind(uris)
            .bind(caller.user)
            .bind(caller.actor())
            .bind(caller.admin)
            .fetch_all(self.store.pool())
            .await?)
    }

    /// Direct children of an entity that the caller may view.
    pub async fn children(&self, caller: &Caller, parent: &str, limit: i64) -> Result<Vec<Entity>> {
        let sql = format!(
            "SELECT {ENTITY_COLUMNS} FROM rookery.entities e \
             WHERE e.parent = $1 AND e.deleted_at IS NULL AND {} \
             ORDER BY e.updated_at DESC LIMIT $5",
            visible_sql("$2", "$3", "$4")
        );
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql.to_string()))
            .bind(parent)
            .bind(caller.user)
            .bind(caller.actor())
            .bind(caller.admin)
            .bind(limit.clamp(1, 1000))
            .fetch_all(self.store.pool())
            .await?)
    }

    /// Search over titles and search text of entities the caller may view, best match first.
    /// Each word matches as a prefix, so this serves mention pickers as well as global search.
    pub async fn search(&self, caller: &Caller, q: &EntitySearch) -> Result<Vec<EntityHit>> {
        let Some(tsq) = prefix_tsquery(&q.text) else {
            // No words: what the caller opened last, then what changed last (a mention picker shows
            // something before the user types).
            let found = self
                .store
                .search()
                .query(
                    caller,
                    &crate::search::SearchQuery {
                        schemes: q.schemes.clone(),
                        spaces: q.space.map(|s| vec![s]),
                        limit: q.limit,
                        ..Default::default()
                    },
                )
                .await?;
            return Ok(found
                .hits
                .into_iter()
                .map(|h| EntityHit {
                    entity: h.entity,
                    rank: 0.0,
                })
                .collect());
        };
        let sql = format!(
            "SELECT {ENTITY_COLUMNS}, {} AS rank \
             FROM rookery.entities e \
             WHERE {} AND NOT {} AND e.deleted_at IS NULL \
               AND ($2::text[] IS NULL OR e.scheme = ANY($2)) \
               AND ($3::uuid IS NULL OR e.space_id = $3) \
               AND {} \
             ORDER BY rank DESC, e.updated_at DESC LIMIT $7",
            crate::documents::rank_sql("e", "to_tsquery('simple', $1)"),
            crate::documents::match_sql("e", "to_tsquery('simple', $1)"),
            crate::documents::attached_sql("e"),
            visible_sql("$4", "$5", "$6")
        );
        let rows: Vec<EntityHitRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql.to_string()))
            .bind(tsq)
            .bind(&q.schemes)
            .bind(q.space)
            .bind(caller.user)
            .bind(caller.actor())
            .bind(caller.admin)
            .bind(q.limit.unwrap_or(20).clamp(1, 200))
            .fetch_all(self.store.pool())
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| EntityHit {
                rank: r.rank,
                entity: r.entity,
            })
            .collect())
    }

    /// Sets the outgoing references of a registered entity for one field (for entities that
    /// are not collection rows; collection rows maintain theirs on write).
    pub async fn set_refs(&self, src: &str, field: &str, targets: &[String]) -> Result<()> {
        let src_uri = Uri::parse(src)?;
        let targets = targets
            .iter()
            .map(|t| Uri::parse(t))
            .collect::<Result<Vec<_>>>()?;
        let mut tx = self.store.pool().begin().await?;
        let source = crate::refs::RefSource {
            scheme: src_uri.scheme.clone(),
            id: src_uri.id.clone(),
            access_uri: src.to_string(),
            owner_id: None,
            plugin: None,
            collection: None,
        };
        crate::refs::replace(&mut tx, &source, field, &targets, false).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Makes `uri` name `target`: resolving it gives the target entity, access to it is the target's, and
    /// references to it are backlinks of the target. For entities that became another (a GitHub issue
    /// converted into a Projects issue), so links, mentions and notifications that name the old URI keep
    /// working. Setting it again replaces the target.
    pub async fn alias(&self, uri: &str, target: &str, plugin: Option<&str>) -> Result<()> {
        Uri::parse(uri)?;
        Uri::parse(target)?;
        if uri == target {
            return Err(crate::error::Error::invalid(
                "an entity cannot be an alias of itself",
            ));
        }
        // No chains: an alias of an alias names the end of it.
        let end: Option<String> =
            sqlx::query_scalar("SELECT target FROM rookery.entity_aliases WHERE uri = $1")
                .bind(target)
                .fetch_optional(self.store.pool())
                .await?;
        let target = end.as_deref().unwrap_or(target);
        sqlx::query(
            "INSERT INTO rookery.entity_aliases (uri, target, plugin) VALUES ($1, $2, $3) \
             ON CONFLICT (uri) DO UPDATE SET target = EXCLUDED.target, plugin = EXCLUDED.plugin",
        )
        .bind(uri)
        .bind(target)
        .bind(plugin)
        .execute(self.store.pool())
        .await?;
        sqlx::query("UPDATE rookery.entity_aliases SET target = $2 WHERE target = $1")
            .bind(uri)
            .bind(target)
            .execute(self.store.pool())
            .await?;
        Ok(())
    }

    /// The entity an alias names, if `uri` is one.
    pub async fn alias_of(&self, uri: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT target FROM rookery.entity_aliases WHERE uri = $1")
                .bind(uri)
                .fetch_optional(self.store.pool())
                .await?,
        )
    }

    /// Whether the caller has at least `level` on the entity.
    pub async fn can(&self, caller: &Caller, uri: &str, level: Level) -> Result<bool> {
        Ok(self
            .store
            .access()
            .level_of(caller, uri)
            .await?
            .is_some_and(|l| l >= level))
    }
}

#[derive(sqlx::FromRow)]
struct EntityHitRow {
    #[sqlx(flatten)]
    entity: Entity,
    rank: f32,
}

/// Builds a prefix tsquery (`word:* & word:*`) from free text, or `None` if it has no words.
pub(crate) fn prefix_tsquery(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(16)
        .map(|w| format!("{}:*", w.to_lowercase()))
        .collect();
    if words.is_empty() {
        None
    } else {
        Some(words.join(" & "))
    }
}

pub(crate) struct CollectionRef<'a> {
    pub plugin: &'a str,
    pub collection: &'a str,
}

/// Inserts or updates an entity; marks inbound references live.
pub(crate) async fn upsert(
    conn: &mut PgConnection,
    rec: &EntityRecord,
    coll: Option<CollectionRef<'_>>,
) -> Result<()> {
    let uri = Uri::parse(&rec.uri)?;
    if let Some(p) = &rec.parent {
        Uri::parse(p)?;
    }
    let title = truncate_str(&rec.title, 4096);
    let search = truncate_str(rec.search_text.as_deref().unwrap_or(""), MAX_SEARCH_TEXT);
    let (plugin, collection) = match coll {
        Some(c) => (Some(c.plugin.to_string()), Some(c.collection.to_string())),
        None => (rec.plugin.clone(), None),
    };
    sqlx::query(
        "INSERT INTO rookery.entities (scheme, id, space_id, parent, title, subtitle, icon, search, \
                                    owner_id, private, plugin, collection) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, \
                 setweight(to_tsvector('simple', $5), 'A') || setweight(to_tsvector('simple', $8), 'B'), \
                 $9, $10, $11, $12) \
         ON CONFLICT (scheme, id) DO UPDATE SET \
            space_id = EXCLUDED.space_id, parent = EXCLUDED.parent, title = EXCLUDED.title, \
            subtitle = EXCLUDED.subtitle, icon = EXCLUDED.icon, search = EXCLUDED.search, \
            owner_id = coalesce(EXCLUDED.owner_id, rookery.entities.owner_id), private = EXCLUDED.private, \
            plugin = EXCLUDED.plugin, collection = EXCLUDED.collection, updated_at = now(), deleted_at = NULL",
    )
    .bind(&uri.scheme)
    .bind(&uri.id)
    .bind(rec.space_id)
    .bind(&rec.parent)
    .bind(title)
    .bind(&rec.subtitle)
    .bind(&rec.icon)
    .bind(search)
    .bind(rec.owner_id)
    .bind(rec.private)
    .bind(plugin)
    .bind(collection)
    .execute(&mut *conn)
    .await?;
    crate::refs::mark_dangling(conn, &uri, false).await
}

pub(crate) async fn fetch(conn: &mut PgConnection, uri: &str) -> Result<Option<Entity>> {
    let sql = format!("SELECT {ENTITY_COLUMNS} FROM rookery.entities e WHERE e.uri = $1");
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql.to_string()))
        .bind(uri)
        .fetch_optional(conn)
        .await?)
}

/// Marks an entity as trashed (or restores it); inbound references dangle while trashed.
pub(crate) async fn set_deleted(conn: &mut PgConnection, uri: &Uri, deleted: bool) -> Result<()> {
    sqlx::query(
        "UPDATE rookery.entities SET deleted_at = CASE WHEN $3 THEN now() ELSE NULL END, updated_at = now() \
         WHERE scheme = $1 AND id = $2",
    )
    .bind(&uri.scheme)
    .bind(&uri.id)
    .bind(deleted)
    .execute(&mut *conn)
    .await?;
    crate::refs::mark_dangling(&mut *conn, uri, deleted).await?;
    crate::refs::set_source_deleted(conn, &uri.scheme, &uri.id, deleted).await
}

/// Deletes an entity from the index with its grants and outgoing references; inbound
/// references are marked dangling.
pub(crate) async fn delete(conn: &mut PgConnection, uri: &str) -> Result<bool> {
    let u = Uri::parse(uri)?;
    let n = sqlx::query("DELETE FROM rookery.entities WHERE scheme = $1 AND id = $2")
        .bind(&u.scheme)
        .bind(&u.id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    sqlx::query("DELETE FROM rookery.grants WHERE resource = $1")
        .bind(uri)
        .execute(&mut *conn)
        .await?;
    crate::refs::delete_from(&mut *conn, &u.scheme, &u.id).await?;
    crate::refs::mark_dangling(conn, &u, true).await?;
    Ok(n > 0)
}
