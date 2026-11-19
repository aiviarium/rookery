//! Search over the entity index for the command palette, the search view and the mention picker: ranked
//! matches in titles and indexed text of what the caller may view, with the facets (matches per type) the
//! search view filters by, and, with no text, what the caller opened last.
//!
//! Cost. A search is one GIN lookup of the prefix query, the rank of the rows it matches, and the
//! visibility check of the best of them in rank order (the check is the same fast path every entity read
//! takes: its parent is a space the caller is in). With 50,000 entities in the index it takes a few
//! milliseconds; `tests/bench_search.rs` measures it. Facets count at most [`FACET_CAP`] matches.

use serde::Serialize;
use uuid::Uuid;

use crate::Store;
use crate::caller::Caller;
use crate::entities::{ENTITY_COLUMNS, Entity, prefix_tsquery, reach_cte, visible_scan_sql};
use crate::error::Result;

/// The most matches a facet count looks at; a count of this many means "at least".
pub const FACET_CAP: i64 = 1000;
/// Without text, padding comes from this many of the newest entities, so the scan stays small.
const PAD_WINDOW: i64 = 300;

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub text: String,
    /// Only these schemes.
    pub schemes: Option<Vec<String>>,
    /// Only entities in these spaces.
    pub spaces: Option<Vec<Uuid>>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// Also count matches per scheme (ignoring `schemes`), for the search view's type filter.
    pub facets: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    #[serde(flatten)]
    pub entity: Entity,
    pub rank: f32,
    /// The title matched; otherwise the text indexed with the entity did.
    pub title_match: bool,
    /// Set for entities the caller opened lately; those lead a search with no text.
    pub recent: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, sqlx::FromRow)]
pub struct Facet {
    pub scheme: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResults {
    pub hits: Vec<SearchHit>,
    /// Per scheme, for a search with text and `facets` set.
    pub facets: Option<Vec<Facet>>,
    /// More matches follow the page.
    pub more: bool,
}

pub struct Search<'a> {
    pub(crate) store: &'a Store,
}

impl<'a> Search<'a> {
    pub async fn query(&self, caller: &Caller, q: &SearchQuery) -> Result<SearchResults> {
        let limit = q.limit.unwrap_or(20).clamp(1, 200);
        let offset = q.offset.unwrap_or(0).max(0);
        let Some(tsq) = prefix_tsquery(&q.text) else {
            return self.browse(caller, q, limit, offset).await;
        };
        let spaces = q.spaces.as_deref();
        let sql = format!(
            "WITH {} \
             SELECT {ENTITY_COLUMNS}, {} AS rank, \
                    to_tsvector('simple', e.title) @@ qq.q AS title_match \
             FROM rookery.entities e, (SELECT to_tsquery('simple', $1) AS q) qq \
             WHERE {} AND NOT {} AND e.deleted_at IS NULL \
               AND ($2::text[] IS NULL OR e.scheme = ANY($2)) \
               AND ($3::uuid[] IS NULL OR e.space_id = ANY($3)) \
               AND {} \
             ORDER BY rank DESC, e.updated_at DESC LIMIT $7 OFFSET $8",
            reach_cte("$4", "$5", "$6"),
            crate::documents::rank_sql("e", "qq.q"),
            crate::documents::match_sql("e", "qq.q"),
            crate::documents::attached_sql("e"),
            visible_scan_sql("$4", "$5", "$6")
        );
        let mut rows: Vec<HitRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql.to_string()))
            .bind(&tsq)
            .bind(&q.schemes)
            .bind(spaces)
            .bind(caller.user)
            .bind(caller.actor())
            .bind(caller.admin)
            .bind(limit + 1)
            .bind(offset)
            .fetch_all(self.store.pool())
            .await?;
        let more = rows.len() as i64 > limit;
        rows.truncate(limit as usize);
        let facets = if q.facets {
            Some(self.facets(caller, &tsq, spaces).await?)
        } else {
            None
        };
        Ok(SearchResults {
            hits: rows
                .into_iter()
                .map(|r| SearchHit {
                    entity: r.entity,
                    rank: r.rank,
                    title_match: r.title_match,
                    recent: false,
                })
                .collect(),
            facets,
            more,
        })
    }

    async fn facets(
        &self,
        caller: &Caller,
        tsq: &str,
        spaces: Option<&[Uuid]>,
    ) -> Result<Vec<Facet>> {
        let sql = format!(
            "WITH {} \
             SELECT scheme, count(*) AS count FROM ( \
                SELECT e.scheme FROM rookery.entities e \
                WHERE {} AND NOT {} AND e.deleted_at IS NULL \
                  AND ($2::uuid[] IS NULL OR e.space_id = ANY($2)) \
                  AND {} \
                LIMIT {FACET_CAP}) m \
             GROUP BY scheme ORDER BY count DESC, scheme",
            reach_cte("$3", "$4", "$5"),
            crate::documents::match_sql("e", "to_tsquery('simple', $1)"),
            crate::documents::attached_sql("e"),
            visible_scan_sql("$3", "$4", "$5")
        );
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql.to_string()))
            .bind(tsq)
            .bind(spaces)
            .bind(caller.user)
            .bind(caller.actor())
            .bind(caller.admin)
            .fetch_all(self.store.pool())
            .await?)
    }

    /// No text: what the caller opened last, then what changed last among what they may view.
    async fn browse(
        &self,
        caller: &Caller,
        q: &SearchQuery,
        limit: i64,
        offset: i64,
    ) -> Result<SearchResults> {
        let want = offset + limit + 1;
        let mut hits: Vec<SearchHit> = Vec::new();
        if caller.user.is_some() {
            let recents = self.store.recents();
            // One space or none; several spaces filter the padding below only.
            let one_space = q
                .spaces
                .as_deref()
                .and_then(|s| (s.len() == 1).then(|| s[0]));
            for r in recents
                .entities(caller, q.schemes.as_deref(), one_space, want)
                .await?
            {
                if q.spaces
                    .as_deref()
                    .is_some_and(|s| r.entity.space_id.is_none_or(|sp| !s.contains(&sp)))
                {
                    continue;
                }
                hits.push(SearchHit {
                    entity: r.entity,
                    rank: 0.0,
                    title_match: false,
                    recent: true,
                });
            }
        }
        if (hits.len() as i64) < want {
            let seen: Vec<String> = hits.iter().map(|h| h.entity.uri.clone()).collect();
            let sql = format!(
                "WITH {} \
                 SELECT {ENTITY_COLUMNS} FROM ( \
                    SELECT * FROM rookery.entities WHERE deleted_at IS NULL \
                      AND ($1::text[] IS NULL OR scheme = ANY($1)) \
                      AND ($2::uuid[] IS NULL OR space_id = ANY($2)) \
                      AND uri <> ALL($3) \
                    ORDER BY updated_at DESC LIMIT {PAD_WINDOW}) e \
                 WHERE NOT {} AND {} ORDER BY e.updated_at DESC LIMIT $7",
                reach_cte("$4", "$5", "$6"),
                crate::documents::attached_sql("e"),
                visible_scan_sql("$4", "$5", "$6")
            );
            let rows: Vec<Entity> = sqlx::query_as(sqlx::AssertSqlSafe(sql.to_string()))
                .bind(&q.schemes)
                .bind(q.spaces.as_deref())
                .bind(&seen)
                .bind(caller.user)
                .bind(caller.actor())
                .bind(caller.admin)
                .bind(want - hits.len() as i64)
                .fetch_all(self.store.pool())
                .await?;
            hits.extend(rows.into_iter().map(|entity| SearchHit {
                entity,
                rank: 0.0,
                title_match: false,
                recent: false,
            }));
        }
        let more = hits.len() as i64 > offset + limit;
        let hits = hits
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect();
        Ok(SearchResults {
            hits,
            facets: None,
            more,
        })
    }
}

#[derive(sqlx::FromRow)]
struct HitRow {
    #[sqlx(flatten)]
    entity: Entity,
    rank: f32,
    title_match: bool,
}
