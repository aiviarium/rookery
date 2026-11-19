//! Idempotent writes. A caller that may retry a write (an app
//! service over rookery.v1, with `crow-idempotency-key`) runs it inside [`scope`]. The collection write it reaches
//! takes the key and records it, with the request's hash and the rows it wrote, in `rookery.idempotency` in the
//! same transaction. A repeat within 24 hours writes nothing and answers with the recorded rows; the same key
//! with another request fails with `INVALID`.
//!
//! Only the first write inside a scope takes the key, so a facility that writes twice for one request cannot
//! answer the second from the first. Other kinds of write record the key in their own transaction with
//! [`record`] and look it up with [`stored`]: enqueueing a job does ([`crate::jobs::Jobs::enqueue_once`]), with
//! the job row and the key committing together. An effect with no row of its own, an event delivered to
//! subscribers, [`claim`]s the key before it happens: it happens at most once.

use std::cell::RefCell;
use std::future::Future;

use serde_json::{Value, json};
use sqlx::{PgConnection, Postgres, Transaction};

use crate::{Error, Result, Store};

/// How long a key is kept.
pub const KEEP_HOURS: i64 = 24;

/// One write's key: whose it is, the key itself, and a hash of the request it came with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    /// `<plugin>.<service>`: keys of different callers never meet.
    pub scope: String,
    pub key: String,
    pub request_hash: String,
}

tokio::task_local! {
    static CURRENT: RefCell<Option<Key>>;
}

/// Runs `f` with `key` for the first write it makes.
pub async fn scope<F: Future>(key: Key, f: F) -> F::Output {
    CURRENT.scope(RefCell::new(Some(key)), f).await
}

/// The key of the current scope, taken so that no later write of the same scope uses it.
pub(crate) fn take() -> Option<Key> {
    CURRENT.try_with(|k| k.borrow_mut().take()).ok().flatten()
}

/// The outcome recorded for `key`, if a live one is: refused with `INVALID` when it was another request's.
pub async fn stored(conn: &mut PgConnection, key: &Key) -> Result<Option<Value>> {
    let row: Option<(String, Value)> = sqlx::query_as(
        "SELECT request_hash, response FROM rookery.idempotency \
         WHERE scope = $1 AND key = $2 AND expires_at > now()",
    )
    .bind(&key.scope)
    .bind(&key.key)
    .fetch_optional(&mut *conn)
    .await?;
    match row {
        None => Ok(None),
        Some((hash, _)) if hash != key.request_hash => Err(Error::invalid(format!(
            "the idempotency key {} was used for another request",
            key.key
        ))),
        Some((_, response)) => Ok(Some(response)),
    }
}

/// Records `response` for `key` in `tx`. `false` when a live record of the key is there already (a
/// concurrent first attempt that committed meanwhile): the caller rolls back and answers from [`stored`].
pub async fn record(
    tx: &mut Transaction<'static, Postgres>,
    key: &Key,
    response: &Value,
) -> Result<bool> {
    let inserted: Option<i32> = sqlx::query_scalar(
        "INSERT INTO rookery.idempotency (scope, key, request_hash, response, expires_at) \
         VALUES ($1, $2, $3, $4, now() + make_interval(hours => $5)) \
         ON CONFLICT (scope, key) DO UPDATE \
             SET request_hash = EXCLUDED.request_hash, response = EXCLUDED.response, \
                 created_at = now(), expires_at = EXCLUDED.expires_at \
             WHERE rookery.idempotency.expires_at <= now() \
         RETURNING 1",
    )
    .bind(&key.scope)
    .bind(&key.key)
    .bind(&key.request_hash)
    .bind(response)
    .bind(KEEP_HOURS as i32)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(inserted.is_some())
}

/// Whether the effect that asked takes place now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// The key is new (or the effect has none): do it.
    Fresh,
    /// The same request came with this key before: do nothing, answer as before.
    Repeat,
}

/// The current scope's key, for an effect that writes no row of its own (publishing an event). The key is
/// recorded in a transaction of its own *before* the effect, so the effect happens at most once: a crash
/// between the two loses it, as events, which are not stored, can be lost anyway, and no retry delivers it
/// twice. Without a scope the effect is [`Claim::Fresh`]. The same key with another request is refused with
/// `INVALID`; a concurrent first attempt that committed the key first makes this one a [`Claim::Repeat`].
pub async fn claim(store: &Store) -> Result<Claim> {
    let Some(key) = take() else {
        return Ok(Claim::Fresh);
    };
    let mut tx = store.pool().begin().await?;
    if stored(&mut tx, &key).await?.is_some() {
        return Ok(Claim::Repeat);
    }
    if record(&mut tx, &key, &json!({})).await? {
        tx.commit().await?;
        return Ok(Claim::Fresh);
    }
    drop(tx);
    // A concurrent attempt recorded it first: a repeat, unless it was another request's.
    let mut conn = store.pool().acquire().await?;
    match stored(&mut conn, &key).await? {
        Some(_) => Ok(Claim::Repeat),
        None => Err(Error::conflict(
            "a write with this idempotency key is in progress",
        )),
    }
}

/// Deletes expired keys; the number deleted.
pub async fn sweep(store: &Store) -> Result<u64> {
    let r = sqlx::query("DELETE FROM rookery.idempotency WHERE expires_at <= now()")
        .execute(store.pool())
        .await?;
    Ok(r.rows_affected())
}

/// The rows of a collection write, as recorded.
pub(crate) fn rows_response(rows: &[Option<Value>]) -> Value {
    json!({ "rows": rows })
}

/// The rows of a batch and the ids of the jobs it enqueued, as recorded: a repeat tells the jobs' followers again.
pub(crate) fn batch_response(rows: &[Option<Value>], jobs: &[uuid::Uuid]) -> Value {
    json!({ "rows": rows, "jobs": jobs })
}

/// The ids of the jobs a recorded batch enqueued (none for a response recorded without them).
pub(crate) fn jobs_of(response: &Value) -> Vec<uuid::Uuid> {
    response["jobs"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()?.parse().ok()).collect())
        .unwrap_or_default()
}

pub(crate) fn rows_of(response: &Value) -> Vec<Option<Value>> {
    response["rows"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|v| if v.is_null() { None } else { Some(v.clone()) })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: &str) -> Key {
        Key {
            scope: "atlas.inference".into(),
            key: k.into(),
            request_hash: "h".into(),
        }
    }

    #[tokio::test]
    async fn only_the_first_write_of_a_scope_takes_the_key() {
        assert_eq!(take(), None);
        scope(key("k1"), async {
            assert_eq!(take(), Some(key("k1")));
            assert_eq!(take(), None);
        })
        .await;
        assert_eq!(take(), None);
    }

    #[test]
    fn recorded_rows_round_trip() {
        let rows = vec![Some(json!({ "id": 1 })), None];
        assert_eq!(rows_of(&rows_response(&rows)), rows);
        assert!(rows_of(&json!({})).is_empty());
        let id = uuid::Uuid::now_v7();
        let r = batch_response(&rows, &[id]);
        assert_eq!(rows_of(&r), rows);
        assert_eq!(jobs_of(&r), vec![id]);
        assert!(jobs_of(&rows_response(&rows)).is_empty());
    }
}
