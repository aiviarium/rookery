//! Key-value storage of plugins, over Rookery's collections `rookery/storage`, `rookery/storageUser`
//! and `rookery/storageSpace`: each key is a row, and its revision is the row's `rev`. A write may
//! name the revision it expects, 0 for a key that is not there yet. Writes go through the
//! collections, so they reach the change log and live queries like any other row.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::Store;
use crate::caller::Caller;
use crate::collections::operations::FieldOp;
use crate::collections::system::PLUGIN;
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub key: String,
    pub value: Value,
    pub revision: i64,
    pub updated_at: DateTime<Utc>,
}

/// Whose a stored value is: the plugin's own, a person's, or a space's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KvScope {
    Global,
    User(Uuid),
    Space(Uuid),
}

impl From<Option<Uuid>> for KvScope {
    fn from(user: Option<Uuid>) -> Self {
        user.map_or(KvScope::Global, KvScope::User)
    }
}

impl KvScope {
    fn collection(self) -> &'static str {
        match self {
            KvScope::Global => "storage",
            KvScope::User(_) => "storageUser",
            KvScope::Space(_) => "storageSpace",
        }
    }

    fn caller(self) -> Caller {
        let c = Caller::system();
        match self {
            KvScope::Global => c,
            KvScope::User(u) => Caller { user: Some(u), ..c },
            KvScope::Space(s) => c.in_space(s),
        }
    }

    /// The condition on the table's scope columns, with the scope's id as `$1`.
    fn sql(self) -> &'static str {
        match self {
            KvScope::Global => "$1::uuid IS NULL",
            KvScope::User(_) => "t.user_id = $1",
            KvScope::Space(_) => "t.space_id = $1",
        }
    }

    fn id(self) -> Option<Uuid> {
        match self {
            KvScope::Global => None,
            KvScope::User(u) | KvScope::Space(u) => Some(u),
        }
    }
}

pub struct Kv<'a> {
    pub(crate) store: &'a Store,
}

fn check_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > 500 {
        return Err(Error::Invalid("a key is 1 to 500 bytes".into()));
    }
    Ok(())
}

type Found = (Uuid, String, Value, i64, DateTime<Utc>);

impl Kv<'_> {
    async fn find(&self, plugin: &str, scope: KvScope, key: &str) -> Result<Option<Found>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT t.id, t.key, coalesce(t.value, 'null'::jsonb), t.rev, t.updated_at FROM rookery.\"{}\" t
             WHERE {} AND t.plugin = $2 AND t.key = $3",
            scope.collection(),
            scope.sql()
        )))
        .bind(scope.id())
        .bind(plugin)
        .bind(key)
        .fetch_optional(self.store.pool())
        .await?)
    }

    pub async fn get(
        &self,
        plugin: &str,
        scope: impl Into<KvScope>,
        key: &str,
    ) -> Result<Option<Entry>> {
        Ok(self.find(plugin, scope.into(), key).await?.map(
            |(_, key, value, revision, updated_at)| Entry {
                key,
                value,
                revision,
                updated_at,
            },
        ))
    }

    /// Sets a value and returns its new revision. With `expected`, only if the key is at that revision.
    pub async fn set(
        &self,
        plugin: &str,
        scope: impl Into<KvScope>,
        key: &str,
        value: &Value,
        expected: Option<i64>,
    ) -> Result<i64> {
        check_key(key)?;
        let scope = scope.into();
        let data = self.store.data(PLUGIN);
        let caller = scope.caller();
        let insert = || async {
            data.insert(
                &caller,
                scope.collection(),
                json!({ "plugin": plugin, "key": key, "value": value }),
            )
            .await
        };
        let bump = |id: Uuid, rev: Option<i64>| {
            let data = self.store.data(PLUGIN);
            let caller = caller.clone();
            async move {
                let ops = [("writes".to_string(), FieldOp::Inc(1.into()))]
                    .into_iter()
                    .collect();
                data.update_ops(
                    &caller,
                    scope.collection(),
                    id,
                    json!({ "value": value }),
                    rev,
                    &ops,
                )
                .await
            }
        };
        let row = match (expected, self.find(plugin, scope, key).await?) {
            (Some(0), Some(_)) => return Err(Error::Conflict(format!("{key} exists already"))),
            (Some(r), None) if r != 0 => {
                return Err(Error::Conflict(format!("{key} is not at revision {r}")));
            }
            (expected, None) => match insert().await {
                // Another set made the key meanwhile: without a revision, this one goes on top.
                Err(e) if e.code() == rookery_proto::ErrorCode::Conflict && expected.is_none() => {
                    let Some((id, ..)) = self.find(plugin, scope, key).await? else {
                        return Err(e);
                    };
                    bump(id, None).await?
                }
                Err(e) if e.code() == rookery_proto::ErrorCode::Conflict => {
                    return Err(Error::Conflict(format!("{key} exists already")));
                }
                other => other?,
            },
            (expected, Some((id, ..))) => bump(id, expected).await.map_err(|e| match e {
                Error::RevConflict { .. } => Error::Conflict(format!(
                    "{key} is not at revision {}",
                    expected.unwrap_or_default()
                )),
                e => e,
            })?,
        };
        row.row
            .as_ref()
            .and_then(|r| r["rev"].as_i64())
            .ok_or_else(|| Error::internal("a stored value without a revision"))
    }

    /// Removes a key; with `expected`, only if it is at that revision.
    pub async fn delete(
        &self,
        plugin: &str,
        scope: impl Into<KvScope>,
        key: &str,
        expected: Option<i64>,
    ) -> Result<bool> {
        let scope = scope.into();
        let Some((id, _, _, revision, _)) = self.find(plugin, scope, key).await? else {
            return Ok(false);
        };
        if let Some(expected) = expected
            && expected != revision
        {
            return Err(Error::Conflict(format!(
                "{key} is not at revision {expected}"
            )));
        }
        self.store
            .data(PLUGIN)
            .delete(&scope.caller(), scope.collection(), id)
            .await?;
        Ok(true)
    }

    /// The keys that start with `prefix`, in order, after `after`.
    pub async fn list(
        &self,
        plugin: &str,
        scope: impl Into<KvScope>,
        prefix: &str,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Entry>> {
        let scope = scope.into();
        let rows: Vec<Found> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT t.id, t.key, coalesce(t.value, 'null'::jsonb), t.rev, t.updated_at FROM rookery.\"{}\" t
             WHERE {} AND t.plugin = $2 AND starts_with(t.key, $3) AND ($4::text IS NULL OR t.key COLLATE \"C\" > $4)
             ORDER BY t.key COLLATE \"C\" LIMIT $5",
            scope.collection(),
            scope.sql()
        )))
        .bind(scope.id())
        .bind(plugin)
        .bind(prefix)
        .bind(after)
        .bind(limit.clamp(1, 1000))
        .fetch_all(self.store.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(_, key, value, revision, updated_at)| Entry {
                key,
                value,
                revision,
                updated_at,
            })
            .collect())
    }
}
