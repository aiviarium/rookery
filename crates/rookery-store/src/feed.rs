//! The change log and the in-process change feed.
//!
//! Every collection write appends to `rookery.changes` in the write's transaction. `seq` is
//! assigned just before commit while holding an advisory lock (and an in-process mutex), so
//! seq order is commit order. After commit, the changes are published with the new row to
//! in-process subscribers from memory, still in seq order. The table is for catching up:
//! [`Feed::changes_after`], and subscribers that lag or see a gap.
//!
//! Writes to Rookery's typed tables reach the log through their trigger, under the same lock, which
//! tells the feed with `NOTIFY rookery_changes`. A listener, started with the first subscription,
//! publishes those from the log.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Postgres, Row, Transaction};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::Store;
use crate::error::{Error, Result};

/// Key of the advisory lock taken while assigning `seq`: the global ordering lock. Log collections never take it.
#[doc(hidden)]
pub const SEQ_LOCK: i64 = 0x726f6f6b5f736571; // "rook_seq"

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeOp {
    Insert,
    Update,
    /// A soft delete (the row is in the trash and `row` has `deletedAt` set) or a hard delete
    /// (`row` is `None`).
    Delete,
    Restore,
}

impl ChangeOp {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeOp::Insert => "insert",
            ChangeOp::Update => "update",
            ChangeOp::Delete => "delete",
            ChangeOp::Restore => "restore",
        }
    }
    fn parse(s: &str) -> ChangeOp {
        match s {
            "insert" => ChangeOp::Insert,
            "update" => ChangeOp::Update,
            "restore" => ChangeOp::Restore,
            _ => ChangeOp::Delete,
        }
    }
}

/// One committed change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub seq: i64,
    pub plugin: String,
    pub collection: String,
    pub id: Uuid,
    pub op: ChangeOp,
    pub rev: i64,
    /// The row's space, for space-scoped collections.
    pub space_id: Option<Uuid>,
    /// The row's user, for user-scoped collections.
    pub user_id: Option<Uuid>,
    pub at: DateTime<Utc>,
    /// The row after the change. From the live feed this is the row as written. When caught up
    /// from the table it is the row as it is now, which may be newer than this change; `None`
    /// when the row no longer exists.
    pub row: Option<Value>,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingChange {
    pub plugin: String,
    pub collection: String,
    pub id: Uuid,
    pub op: ChangeOp,
    pub rev: i64,
    pub space_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub row: Option<Value>,
}

pub(crate) struct FeedState {
    tx: broadcast::Sender<Arc<Change>>,
    /// The last seq published from this process.
    head: Mutex<i64>,
    /// Serialises "assign seq, commit, publish" within this process.
    write_lock: tokio::sync::Mutex<()>,
    /// Whether the listener for changes of typed tables runs.
    listening: std::sync::atomic::AtomicBool,
}

impl FeedState {
    pub(crate) fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(16));
        FeedState {
            tx,
            head: Mutex::new(0),
            write_lock: tokio::sync::Mutex::new(()),
            listening: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

/// What a subscriber receives.
#[derive(Debug, Clone)]
pub enum FeedEvent {
    Change(Arc<Change>),
    /// The subscriber's cursor is older than the retained log. Refetch everything, then
    /// continue from `head`.
    Reset {
        head: i64,
    },
}

/// The result of [`Feed::changes_after`].
#[derive(Debug, Clone)]
pub enum CatchUp {
    Changes(Vec<Change>),
    Reset { head: i64 },
}

/// Commits a write transaction, assigning `seq` to its changes in commit order and publishing
/// them afterwards. Runs on its own task so that a caller going away cannot leave a committed
/// change unpublished.
pub(crate) async fn commit(
    store: &Store,
    mut tx: Transaction<'static, Postgres>,
    pending: Vec<PendingChange>,
    fence: Option<(
        String,
        crate::collections::conditions::BatchCondition,
        String,
    )>,
) -> Result<Vec<Arc<Change>>> {
    if pending.is_empty() {
        if let Some((plugin, condition, holder)) = &fence {
            crate::collections::check_condition(&mut tx, plugin, Some(condition), holder).await?;
        }
        tx.commit().await?;
        return Ok(Vec::new());
    }
    let store = store.clone();
    tokio::spawn(async move { commit_inner(&store, tx, pending, fence).await })
        .await
        .map_err(|e| Error::internal(format!("commit task failed: {e}")))?
}

async fn commit_inner(
    store: &Store,
    mut tx: Transaction<'static, Postgres>,
    pending: Vec<PendingChange>,
    fence: Option<(
        String,
        crate::collections::conditions::BatchCondition,
        String,
    )>,
) -> Result<Vec<Arc<Change>>> {
    let state = &store.data.feed;
    let _guard = state.write_lock.lock().await;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(SEQ_LOCK)
        .execute(&mut *tx)
        .await?;
    let n = pending.len() as i64;
    let last: i64 =
        sqlx::query_scalar("UPDATE rookery.change_counter SET seq = seq + $1 RETURNING seq")
            .bind(n)
            .fetch_one(&mut *tx)
            .await?;
    let first = last - n + 1;
    let seqs: Vec<i64> = (first..=last).collect();
    let at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO rookery.changes (seq, plugin, collection, id, op, rev, space_id, user_id) \
         SELECT * FROM UNNEST($1::bigint[], $2::text[], $3::text[], $4::uuid[], $5::text[], \
                              $6::bigint[], $7::uuid[], $8::uuid[]) \
         RETURNING at",
    )
    .bind(&seqs)
    .bind(pending.iter().map(|p| p.plugin.clone()).collect::<Vec<_>>())
    .bind(
        pending
            .iter()
            .map(|p| p.collection.clone())
            .collect::<Vec<_>>(),
    )
    .bind(pending.iter().map(|p| p.id).collect::<Vec<_>>())
    .bind(
        pending
            .iter()
            .map(|p| p.op.as_str().to_string())
            .collect::<Vec<_>>(),
    )
    .bind(pending.iter().map(|p| p.rev).collect::<Vec<_>>())
    .bind(pending.iter().map(|p| p.space_id).collect::<Vec<_>>())
    .bind(pending.iter().map(|p| p.user_id).collect::<Vec<_>>())
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .next()
    .unwrap_or_else(Utc::now);
    if let Some((plugin, condition, holder)) = &fence {
        crate::collections::check_condition(&mut tx, plugin, Some(condition), holder).await?;
    }
    tx.commit().await?;

    let changes: Vec<Arc<Change>> = pending
        .into_iter()
        .zip(seqs)
        .map(|(p, seq)| {
            Arc::new(Change {
                seq,
                plugin: p.plugin,
                collection: p.collection,
                id: p.id,
                op: p.op,
                rev: p.rev,
                space_id: p.space_id,
                user_id: p.user_id,
                at,
                row: p.row,
            })
        })
        .collect();
    let mut head = state.head.lock().expect("feed head lock");
    for c in &changes {
        *head = (*head).max(c.seq);
        // No receivers is fine.
        let _ = state.tx.send(c.clone());
    }
    Ok(changes)
}

/// The change feed.
pub struct Feed<'a> {
    pub(crate) store: &'a Store,
}

impl<'a> Feed<'a> {
    pub(crate) async fn init_head(&self) -> Result<()> {
        let seq: i64 = sqlx::query_scalar("SELECT seq FROM rookery.change_counter")
            .fetch_one(self.store.pool())
            .await?;
        let mut head = self.store.data.feed.head.lock().expect("feed head lock");
        *head = (*head).max(seq);
        Ok(())
    }

    /// The last seq published in this process.
    pub fn head(&self) -> i64 {
        *self.store.data.feed.head.lock().expect("feed head lock")
    }

    /// The last seq in the change log, whichever process or trigger wrote it.
    pub async fn logged_head(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT seq FROM rookery.change_counter")
            .fetch_one(self.store.pool())
            .await?)
    }

    /// Active in-process subscriptions, for diagnostics.
    pub fn subscription_count(&self) -> usize {
        self.store.data.feed.tx.receiver_count()
    }

    /// Subscribes to changes committed from now on: after the head of the log, which may be ahead of
    /// what this process published when a typed table's trigger wrote it.
    pub async fn subscribe(&self) -> Result<Subscription> {
        self.listen();
        let state = &self.store.data.feed;
        let rx = state.tx.subscribe();
        let logged: i64 = sqlx::query_scalar("SELECT seq FROM rookery.change_counter")
            .fetch_one(self.store.pool())
            .await?;
        let head = *state.head.lock().expect("feed head lock");
        Ok(Subscription {
            store: self.store.clone(),
            rx,
            last: head.max(logged),
            buffer: VecDeque::new(),
        })
    }

    /// A subscription from this process's head, for code that cannot wait.
    fn subscribe_here(&self) -> Subscription {
        self.listen();
        let state = &self.store.data.feed;
        let head = state.head.lock().expect("feed head lock");
        let rx = state.tx.subscribe();
        Subscription {
            store: self.store.clone(),
            rx,
            last: *head,
            buffer: VecDeque::new(),
        }
    }

    /// Subscribes to every change after `after`: first those in the log, then live ones.
    /// The first event is a [`FeedEvent::Reset`] if `after` is older than the retained log.
    pub fn subscribe_from(&self, after: i64) -> Subscription {
        let mut sub = self.subscribe_here();
        sub.last = after;
        sub
    }

    /// Starts, once, the task that publishes the changes the triggers of typed tables record. It ends
    /// when the store's pool closes.
    fn listen(&self) {
        use std::sync::atomic::Ordering;
        let state = &self.store.data.feed;
        if state.listening.swap(true, Ordering::SeqCst)
            || tokio::runtime::Handle::try_current().is_err()
        {
            return;
        }
        let store = self.store.clone();
        tokio::spawn(async move {
            let Ok(mut listener) = sqlx::postgres::PgListener::connect_with(store.pool()).await
            else {
                store.data.feed.listening.store(false, Ordering::SeqCst);
                return;
            };
            if listener
                .listen(crate::collections::system::CHANNEL)
                .await
                .is_err()
            {
                store.data.feed.listening.store(false, Ordering::SeqCst);
                return;
            }
            loop {
                match listener.try_recv().await {
                    Ok(Some(_)) => {}
                    // The connection dropped: what was told meanwhile is in the log.
                    Ok(None) => {}
                    Err(_) if store.pool().is_closed() => break,
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        continue;
                    }
                }
                if store.pool().is_closed() {
                    break;
                }
                if let Err(e) = store.feed().publish_logged().await {
                    tracing::warn!(error = %e, "publishing changes from the change log failed");
                }
            }
            store.data.feed.listening.store(false, Ordering::SeqCst);
        });
    }

    /// Publishes the changes in the log after this process's head, as the trigger of a typed table
    /// recorded them.
    async fn publish_logged(&self) -> Result<()> {
        loop {
            let head = self.head();
            let changes = match self.changes_after(head, 1000).await? {
                CatchUp::Changes(c) => c,
                CatchUp::Reset { head } => {
                    let mut h = self.store.data.feed.head.lock().expect("feed head lock");
                    *h = (*h).max(head);
                    return Ok(());
                }
            };
            if changes.is_empty() {
                return Ok(());
            }
            let state = &self.store.data.feed;
            let mut h = state.head.lock().expect("feed head lock");
            for c in changes {
                if c.seq <= *h {
                    continue;
                }
                *h = c.seq;
                let _ = state.tx.send(Arc::new(c));
            }
        }
    }

    /// The changes after `after` from the log, at most `limit`, with the current rows.
    pub async fn changes_after(&self, after: i64, limit: i64) -> Result<CatchUp> {
        let row = sqlx::query("SELECT seq, trimmed_through FROM rookery.change_counter")
            .fetch_one(self.store.pool())
            .await?;
        let head: i64 = row.try_get("seq")?;
        let trimmed: i64 = row.try_get("trimmed_through")?;
        // Older than the retained log, or ahead of it (a cursor from before a restore).
        if after < trimmed || after > head {
            return Ok(CatchUp::Reset { head });
        }
        let rows = sqlx::query(
            "SELECT seq, plugin, collection, id, op, rev, space_id, user_id, at \
             FROM rookery.changes WHERE seq > $1 ORDER BY seq LIMIT $2",
        )
        .bind(after)
        .bind(limit.clamp(1, 10_000))
        .fetch_all(self.store.pool())
        .await?;
        let mut changes = Vec::with_capacity(rows.len());
        for r in rows {
            let op: String = r.try_get("op")?;
            changes.push(Change {
                seq: r.try_get("seq")?,
                plugin: r.try_get("plugin")?,
                collection: r.try_get("collection")?,
                id: r.try_get("id")?,
                op: ChangeOp::parse(&op),
                rev: r.try_get("rev")?,
                space_id: r.try_get("space_id")?,
                user_id: r.try_get("user_id")?,
                at: r.try_get("at")?,
                row: None,
            });
        }
        // Seqs have no gaps, so a first change past `after + 1` means the log was trimmed
        // between the two reads.
        match changes.first() {
            Some(c) if c.seq != after + 1 => return Ok(CatchUp::Reset { head }),
            None if head > after => {
                // Everything after `after` was trimmed, or committed after our first read.
                let trimmed: i64 =
                    sqlx::query_scalar("SELECT trimmed_through FROM rookery.change_counter")
                        .fetch_one(self.store.pool())
                        .await?;
                if after < trimmed {
                    return Ok(CatchUp::Reset { head });
                }
            }
            _ => {}
        }
        self.load_rows(&mut changes).await?;
        Ok(CatchUp::Changes(changes))
    }

    async fn load_rows(&self, changes: &mut [Change]) -> Result<()> {
        let mut by_collection: HashMap<(String, String), Vec<Uuid>> = HashMap::new();
        for c in changes.iter() {
            by_collection
                .entry((c.plugin.clone(), c.collection.clone()))
                .or_default()
                .push(c.id);
        }
        let mut rows: HashMap<Uuid, Value> = HashMap::new();
        for ((plugin, collection), ids) in by_collection {
            let Ok(coll) = self.store.collections().get(&plugin, &collection) else {
                continue;
            };
            for (id, row) in crate::collections::load_rows_by_id(self.store, &coll, &ids).await? {
                rows.insert(id, row);
            }
        }
        for c in changes.iter_mut() {
            c.row = rows.get(&c.id).cloned();
        }
        Ok(())
    }

    /// Deletes changes older than `keep`. Subscribers behind the trimmed point get a reset.
    pub async fn trim(&self, keep: Duration) -> Result<u64> {
        let cutoff = Utc::now()
            - chrono::Duration::from_std(keep).map_err(|e| Error::invalid(e.to_string()))?;
        let through: Option<i64> =
            sqlx::query_scalar("SELECT max(seq) FROM rookery.changes WHERE at < $1")
                .bind(cutoff)
                .fetch_one(self.store.pool())
                .await?;
        match through {
            Some(seq) => self.trim_through(seq).await,
            None => Ok(0),
        }
    }

    /// Deletes changes up to and including `seq`.
    pub async fn trim_through(&self, seq: i64) -> Result<u64> {
        let mut tx = self.store.pool().begin().await?;
        let n = sqlx::query("DELETE FROM rookery.changes WHERE seq <= $1")
            .bind(seq)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        sqlx::query(
            "UPDATE rookery.change_counter SET trimmed_through = greatest(trimmed_through, least($1, seq))",
        )
        .bind(seq)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(n)
    }
}

/// A subscription to the feed. Delivers every change after its cursor exactly once, in seq
/// order: from memory when it keeps up, from the log when it lags or sees a gap (a change
/// committed by another process, or a publish it missed).
pub struct Subscription {
    store: Store,
    rx: broadcast::Receiver<Arc<Change>>,
    last: i64,
    buffer: VecDeque<Arc<Change>>,
}

impl Subscription {
    /// The seq of the last change delivered (the cursor to resume from).
    pub fn last_seq(&self) -> i64 {
        self.last
    }

    /// The next event. Returns `None` when the store is gone.
    pub async fn next(&mut self) -> Result<Option<FeedEvent>> {
        loop {
            if let Some(c) = self.buffer.pop_front() {
                if c.seq <= self.last {
                    continue;
                }
                self.last = c.seq;
                return Ok(Some(FeedEvent::Change(c)));
            }
            // Changes may already be ahead of our cursor (subscribe_from, or a gap). Prefer
            // memory if the next one is there; otherwise read the log.
            // (A cursor ahead of this process's head is checked against the log too.)
            if self.store.feed().head() != self.last {
                let next = match self.rx.try_recv() {
                    Ok(c) => Some(c),
                    Err(broadcast::error::TryRecvError::Closed) => return Ok(None),
                    Err(_) => None,
                };
                match next {
                    Some(c) if c.seq <= self.last => continue,
                    Some(c) if c.seq == self.last + 1 => {
                        self.last = c.seq;
                        return Ok(Some(FeedEvent::Change(c)));
                    }
                    other => {
                        if let Some(reset) = self.catch_up(other).await? {
                            return Ok(Some(reset));
                        }
                        if !self.buffer.is_empty() {
                            continue;
                        }
                    }
                }
            }
            match self.rx.recv().await {
                Ok(c) if c.seq <= self.last => continue,
                Ok(c) if c.seq == self.last + 1 => {
                    self.last = c.seq;
                    return Ok(Some(FeedEvent::Change(c)));
                }
                Ok(c) => {
                    if let Some(reset) = self.catch_up(Some(c)).await? {
                        return Ok(Some(reset));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if let Some(reset) = self.catch_up(None).await? {
                        return Ok(Some(reset));
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(None),
            }
        }
    }

    /// Fills the buffer from the log, up to and including `upto` when given.
    async fn catch_up(&mut self, upto: Option<Arc<Change>>) -> Result<Option<FeedEvent>> {
        match self.store.feed().changes_after(self.last, 1000).await? {
            CatchUp::Reset { head } => {
                self.buffer.clear();
                // Skip whatever is buffered in the channel up to the new head.
                self.last = head;
                Ok(Some(FeedEvent::Reset { head }))
            }
            CatchUp::Changes(changes) => {
                // Changes from memory may arrive before the log read sees them; `upto` is
                // kept only when it directly follows what the log returned. Otherwise the
                // next catch-up reads it from the log.
                let limit = upto.as_ref().map(|c| c.seq);
                let mut expected = self.last;
                for c in changes {
                    if limit.is_some_and(|l| c.seq >= l) {
                        break;
                    }
                    expected = c.seq;
                    self.buffer.push_back(Arc::new(c));
                }
                if let Some(c) = upto
                    && c.seq == expected + 1
                {
                    self.buffer.push_back(c);
                }
                Ok(None)
            }
        }
    }
}
