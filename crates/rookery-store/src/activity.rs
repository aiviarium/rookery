//! Recent activity: what changed lately in the spaces a user is in, from the change log, with the fields
//! each change touched from the history of collections that keep one. Only entities the caller may view
//! appear, and only rows of collections that are entities (a change to a row that is not an entity has
//! nothing to show or open).
//!
//! Cost. Per space one backwards range scan of `changes_space_seq` for `limit * 4` changes (the extra is
//! what collapsing runs of edits and hiding what the caller may not see eat), one probe of
//! `entities_collection_id` per change, one of the history's index and the user table, and the visibility
//! check on the rows that survive, so about `spaces * limit * 4` index probes, whatever the size of the
//! log. `tests/bench_search.rs` measures it with 50,000 changes.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::Store;
use crate::caller::{Caller, Level};
use crate::entities::{ENTITY_COLUMNS, Entity, visible_sql};
use crate::error::Result;

/// Edits to one entity by one person this close together are one entry.
const COLLAPSE_SECS: i64 = 15 * 60;
/// Changes older than this are not shown.
const HORIZON_DAYS: i32 = 30;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityItem {
    /// The seq of the newest change in the entry.
    pub seq: i64,
    pub at: DateTime<Utc>,
    /// `insert`, `update`, `delete` or `restore`; a run that began with an insert is an `insert`.
    pub op: String,
    pub actor_id: Option<Uuid>,
    pub actor_name: Option<String>,
    pub entity: Entity,
    /// The fields changed, where the collection keeps history.
    pub fields: Vec<String>,
    /// How many changes the entry stands for.
    pub changes: u32,
}

pub struct ActivityFeed<'a> {
    pub(crate) store: &'a Store,
}

impl<'a> ActivityFeed<'a> {
    /// The newest `limit` entries across the spaces the caller is in, or in `space`.
    pub async fn recent(
        &self,
        caller: &Caller,
        space: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<ActivityItem>> {
        let limit = limit.clamp(1, 100);
        let spaces: Vec<Uuid> = match space {
            Some(s) => {
                self.store
                    .access()
                    .require(caller, &format!("space://{s}"), Level::View)
                    .await?;
                vec![s]
            }
            None => self
                .store
                .spaces()
                .list_for(caller)
                .await?
                .into_iter()
                .map(|s| s.id)
                .collect(),
        };
        if spaces.is_empty() {
            return Ok(Vec::new());
        }
        let fetch = limit * 4;
        let sql = format!(
            "WITH c AS ( \
                SELECT c.* FROM unnest($1::uuid[]) s(id) CROSS JOIN LATERAL ( \
                    SELECT * FROM rookery.changes c WHERE c.space_id = s.id \
                      AND c.at > now() - make_interval(days => {HORIZON_DAYS}) \
                    ORDER BY c.seq DESC LIMIT $2) c \
                ORDER BY c.seq DESC LIMIT $2) \
             SELECT a.seq, a.at, a.op, u.id AS actor_id, u.name AS actor_name, a.fields, {ENTITY_COLUMNS} \
             FROM ( \
                SELECT c.seq, c.at, c.op, c.plugin, c.collection, c.id, h.changes AS fields, \
                       coalesce(h.on_behalf_of, \
                                CASE WHEN h.principal ~ '^user:[0-9a-f-]{{36}}$' \
                                     THEN substr(h.principal, 6)::uuid END) AS actor_id \
                FROM c LEFT JOIN rookery.history h ON h.plugin = c.plugin AND h.collection = c.collection \
                     AND h.row_id = c.id AND h.rev = c.rev) a \
             JOIN rookery.entities e ON e.plugin = a.plugin AND e.collection = a.collection AND e.id = a.id::text \
             LEFT JOIN rookery.users u ON u.id = coalesce(a.actor_id, CASE WHEN a.op = 'insert' THEN e.owner_id END) \
             WHERE {} \
             ORDER BY a.seq DESC",
            visible_sql("$3", "$4", "$5")
        );
        let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql.to_string()))
            .bind(&spaces)
            .bind(fetch)
            .bind(caller.user)
            .bind(caller.actor())
            .bind(caller.admin)
            .fetch_all(self.store.pool())
            .await?;
        Ok(collapse(rows, limit as usize))
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    seq: i64,
    at: DateTime<Utc>,
    op: String,
    actor_id: Option<Uuid>,
    actor_name: Option<String>,
    fields: Option<Value>,
    #[sqlx(flatten)]
    entity: Entity,
}

/// Newest first in, newest first out: a change joins the entry of the same person and entity that is
/// within [`COLLAPSE_SECS`] of it.
fn collapse(rows: Vec<Row>, limit: usize) -> Vec<ActivityItem> {
    let mut out: Vec<ActivityItem> = Vec::new();
    for r in rows {
        let fields: Vec<String> = r
            .fields
            .as_ref()
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let joined = out.iter_mut().find(|g| {
            g.entity.uri == r.entity.uri
                && g.actor_id == r.actor_id
                && (g.at - r.at).num_seconds().abs() <= COLLAPSE_SECS
        });
        if let Some(g) = joined {
            g.changes += 1;
            for f in fields {
                if !g.fields.contains(&f) {
                    g.fields.push(f);
                }
            }
            // The run began with this older change.
            if r.op == "insert" {
                g.op = "insert".into();
            }
            g.at = g.at.max(r.at);
            continue;
        }
        if out.len() >= limit {
            continue;
        }
        out.push(ActivityItem {
            seq: r.seq,
            at: r.at,
            op: r.op,
            actor_id: r.actor_id,
            actor_name: r.actor_name,
            entity: r.entity,
            fields,
            changes: 1,
        });
    }
    out
}
