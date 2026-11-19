//! A person's layouts of a shell, by name, over Rookery's collection `rookery/userLayouts`: the
//! revision of a layout is its row's `rev`, and a write may name the one it expects (0: new).

use serde_json::{Value, json};
use uuid::Uuid;

use crate::Store;
use crate::caller::Caller;
use crate::collections::system::PLUGIN;
use crate::error::{Error, Result};

const COLLECTION: &str = "userLayouts";

pub struct Layout<'a> {
    pub(crate) store: &'a Store,
}

impl Layout<'_> {
    async fn find(&self, user: Uuid, name: &str) -> Result<Option<(Uuid, Value, i64)>> {
        Ok(sqlx::query_as(
            "SELECT id, coalesce(value, 'null'::jsonb), rev FROM rookery.\"userLayouts\" WHERE user_id = $1 AND name = $2",
        )
        .bind(user)
        .bind(name)
        .fetch_optional(self.store.pool())
        .await?)
    }

    pub async fn get(&self, user: Uuid, name: &str) -> Result<Option<(Value, i64)>> {
        Ok(self.find(user, name).await?.map(|(_, v, r)| (v, r)))
    }

    /// Writes a layout and returns its revision. With `expected`, only if it is at that one (0: new).
    pub async fn put(
        &self,
        user: Uuid,
        name: &str,
        value: &Value,
        expected: Option<i64>,
    ) -> Result<i64> {
        let caller = Caller {
            user: Some(user),
            ..Caller::system()
        };
        let data = self.store.data(PLUGIN);
        let out = match (expected, self.find(user, name).await?) {
            (Some(0), Some(_)) => return Err(Error::Conflict("the layout exists already".into())),
            (Some(r), None) if r != 0 => {
                return Err(Error::Conflict(format!(
                    "the layout is not at revision {r}"
                )));
            }
            (_, None) => data
                .insert(&caller, COLLECTION, json!({ "name": name, "value": value }))
                .await
                .map_err(|e| {
                    if e.code() == rookery_proto::ErrorCode::Conflict {
                        Error::Conflict("the layout exists already".into())
                    } else {
                        e
                    }
                })?,
            (expected, Some((id, ..))) => data
                .update(&caller, COLLECTION, id, json!({ "value": value }), expected)
                .await
                .map_err(|e| match e {
                    Error::RevConflict { expected, .. } => {
                        Error::Conflict(format!("the layout is not at revision {expected}"))
                    }
                    e => e,
                })?,
        };
        out.row
            .as_ref()
            .and_then(|r| r["rev"].as_i64())
            .ok_or_else(|| Error::internal("a layout without a revision"))
    }
}
