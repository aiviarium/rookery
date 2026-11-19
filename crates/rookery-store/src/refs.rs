//! The reference index: `ref` and `refs` fields and mentions inside richtext, across all
//! plugins. Backlinks are filtered by what the caller may see.

use serde::Serialize;
use sqlx::{AssertSqlSafe, PgConnection};
use uuid::Uuid;

use crate::Store;
use crate::caller::Caller;
use crate::entities::visible_as;
use crate::error::Result;
use crate::uri::Uri;

/// The source of references. For rows of entity collections and registered entities this is
/// the entity; for rows of other collections the scheme is `row` and the id is
/// `<plugin>/<collection>/<row id>`.
#[derive(Debug, Clone)]
pub(crate) struct RefSource {
    pub scheme: String,
    pub id: String,
    /// Whose view level decides whether a reference is visible.
    pub access_uri: String,
    /// Only this user may see the references (user-scoped rows that are not entities).
    pub owner_id: Option<Uuid>,
    pub plugin: Option<String>,
    pub collection: Option<String>,
}

/// A reference pointing at an entity.
#[derive(Debug, Clone, Serialize, PartialEq, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Backlink {
    /// The source's URI. For rows that are not entities: `row://<plugin>/<collection>/<id>`.
    pub source: String,
    /// The field the reference is in.
    pub field: String,
    pub plugin: Option<String>,
    pub collection: Option<String>,
    /// The source row's id, when the source is a collection row.
    pub row_id: Option<Uuid>,
    /// The source's title from the entity index, when it is an entity.
    pub title: Option<String>,
    pub icon: Option<String>,
    /// The source's subtitle from the entity index (a state, say), when it is an entity.
    pub subtitle: Option<String>,
    /// When the reference was first made. Rewriting the source keeps it while the reference stays.
    pub at: chrono::DateTime<chrono::Utc>,
}

/// A reference going out of an entity.
#[derive(Debug, Clone, Serialize, PartialEq, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Reference {
    pub field: String,
    pub target: String,
    pub dangling: bool,
    pub title: Option<String>,
    pub icon: Option<String>,
}

pub struct Refs<'a> {
    pub(crate) store: &'a Store,
}

// The source entity and the resource that controls the reference both have to be readable.
const SOURCE_JOINS: &str =
    "LEFT JOIN rookery.entities src ON src.scheme = r.src_scheme AND src.id = r.src_id
     LEFT JOIN rookery.entities access ON access.uri = r.access_uri";

fn source_visible() -> String {
    format!(
        "($5 OR (
            (src.uri IS NULL OR src.uri = access.uri OR {source}) AND
            CASE WHEN access.uri IS NOT NULL AND r.src_scheme <> 'row' THEN {access}
                 WHEN rookery.collection_need(r.plugin, r.collection) > 3 THEN false
                 WHEN r.owner_id IS NOT NULL THEN r.owner_id = $3
                 ELSE rookery.effective_level(r.access_uri, $3, rookery.principals_of($3, $4))
                      >= rookery.collection_need(r.plugin, r.collection) END
        ))",
        source = visible_as("src", "$3", "$4", "$5", "TRUE"),
        access = visible_as("access", "$3", "$4", "$5", "TRUE"),
    )
}

impl<'a> Refs<'a> {
    /// Backlinks from readable sources, including ones pointing to an alias of the target.
    pub async fn to(&self, caller: &Caller, uri: &str) -> Result<Vec<Backlink>> {
        let u = Uri::parse(uri)?;
        let sql = format!(
            "SELECT r.src_scheme || '://' || r.src_id AS source, r.field, r.plugin, r.collection,
                    CASE WHEN r.src_scheme = 'row' THEN split_part(r.src_id, '/', 3)::uuid
                         WHEN src.collection IS NOT NULL THEN r.src_id::uuid END AS row_id,
                    src.title, src.icon, src.subtitle, r.created_at AS at
             FROM rookery.refs r
             {SOURCE_JOINS}
             WHERE ((r.dst_scheme = $1 AND r.dst_id = $2)
                    OR r.dst_scheme || '://' || r.dst_id IN
                       (SELECT a.uri FROM rookery.entity_aliases a WHERE a.target = $1 || '://' || $2))
               AND NOT EXISTS (
                   SELECT 1 FROM rookery.entities target
                   WHERE target.uri = coalesce(
                       (SELECT a.target FROM rookery.entity_aliases a WHERE a.uri = $1 || '://' || $2),
                       $1 || '://' || $2)
                     AND NOT {target_visible})
               AND NOT r.src_deleted AND {source_visible}
             ORDER BY r.created_at DESC, source, r.field LIMIT 1000",
            target_visible = visible_as("target", "$3", "$4", "$5", "TRUE"),
            source_visible = source_visible(),
        );
        Ok(sqlx::query_as(AssertSqlSafe(sql))
            .bind(&u.scheme)
            .bind(&u.id)
            .bind(caller.user)
            .bind(caller.actor())
            .bind(caller.admin)
            .fetch_all(self.store.pool())
            .await?)
    }

    /// The references from a readable source. An unreadable target looks unresolved.
    pub async fn from(&self, caller: &Caller, uri: &str) -> Result<Vec<Reference>> {
        let u = Uri::parse(uri)?;
        let sql = format!(
            "SELECT r.field, r.dst_scheme || '://' || r.dst_id AS target,
                    dst.uri IS NULL OR dst.deleted_at IS NOT NULL AS dangling, dst.title, dst.icon
             FROM rookery.refs r
             {SOURCE_JOINS}
             LEFT JOIN rookery.entity_aliases a ON a.uri = r.dst_scheme || '://' || r.dst_id
             LEFT JOIN rookery.entities dst ON dst.uri = coalesce(a.target, r.dst_scheme || '://' || r.dst_id)
                 AND {target_visible}
             WHERE r.src_scheme = $1 AND r.src_id = $2 AND NOT r.src_deleted AND {source_visible}
             ORDER BY r.field, target",
            target_visible = visible_as("dst", "$3", "$4", "$5", "TRUE"),
            source_visible = source_visible(),
        );
        Ok(sqlx::query_as(AssertSqlSafe(sql))
            .bind(&u.scheme)
            .bind(&u.id)
            .bind(caller.user)
            .bind(caller.actor())
            .bind(caller.admin)
            .fetch_all(self.store.pool())
            .await?)
    }
}

/// Replaces the references of `src` in `field` with `targets`. `fresh` skips removing old
/// ones (the source was just created). References that stay keep their row, and so the time they
/// were first made; their access and ownership follow the source.
pub(crate) async fn replace(
    conn: &mut PgConnection,
    src: &RefSource,
    field: &str,
    targets: &[Uri],
    fresh: bool,
) -> Result<()> {
    if !fresh {
        let schemes: Vec<&str> = targets.iter().map(|t| t.scheme.as_str()).collect();
        let ids: Vec<&str> = targets.iter().map(|t| t.id.as_str()).collect();
        sqlx::query(
            "DELETE FROM rookery.refs WHERE src_scheme = $1 AND src_id = $2 AND field = $3 \
             AND (dst_scheme, dst_id) NOT IN (SELECT * FROM unnest($4::text[], $5::text[]))",
        )
        .bind(&src.scheme)
        .bind(&src.id)
        .bind(field)
        .bind(&schemes)
        .bind(&ids)
        .execute(&mut *conn)
        .await?;
    }
    if targets.is_empty() {
        return Ok(());
    }
    let schemes: Vec<&str> = targets.iter().map(|t| t.scheme.as_str()).collect();
    let ids: Vec<&str> = targets.iter().map(|t| t.id.as_str()).collect();
    sqlx::query(
        "INSERT INTO rookery.refs (src_scheme, src_id, field, dst_scheme, dst_id, access_uri, owner_id, plugin, collection, dangling) \
         SELECT $1, $2, $3, t.s, t.i, $4, $9, $5, $6, \
                NOT EXISTS (SELECT 1 FROM rookery.entities e WHERE e.scheme = t.s AND e.id = t.i AND e.deleted_at IS NULL) \
         FROM unnest($7::text[], $8::text[]) AS t(s, i) \
         ON CONFLICT (src_scheme, src_id, field, dst_scheme, dst_id) DO UPDATE \
         SET access_uri = EXCLUDED.access_uri, owner_id = EXCLUDED.owner_id, \
             plugin = EXCLUDED.plugin, collection = EXCLUDED.collection",
    )
    .bind(&src.scheme)
    .bind(&src.id)
    .bind(field)
    .bind(&src.access_uri)
    .bind(&src.plugin)
    .bind(&src.collection)
    .bind(&schemes)
    .bind(&ids)
    .bind(src.owner_id)
    .execute(conn)
    .await?;
    Ok(())
}

pub(crate) async fn mark_dangling(
    conn: &mut PgConnection,
    dst: &Uri,
    dangling: bool,
) -> Result<()> {
    sqlx::query(
        "UPDATE rookery.refs SET dangling = $3 WHERE dst_scheme = $1 AND dst_id = $2 AND dangling <> $3",
    )
    .bind(&dst.scheme)
    .bind(&dst.id)
    .bind(dangling)
    .execute(conn)
    .await?;
    Ok(())
}

pub(crate) async fn set_source_deleted(
    conn: &mut PgConnection,
    scheme: &str,
    id: &str,
    deleted: bool,
) -> Result<()> {
    sqlx::query("UPDATE rookery.refs SET src_deleted = $3 WHERE src_scheme = $1 AND src_id = $2")
        .bind(scheme)
        .bind(id)
        .bind(deleted)
        .execute(conn)
        .await?;
    Ok(())
}

pub(crate) async fn delete_from(conn: &mut PgConnection, scheme: &str, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM rookery.refs WHERE src_scheme = $1 AND src_id = $2")
        .bind(scheme)
        .bind(id)
        .execute(conn)
        .await?;
    Ok(())
}
