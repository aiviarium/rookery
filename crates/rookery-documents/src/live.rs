//! The kernel's replica of a document while anyone uses it: one task per document that owns the `yrs`
//! document and the presence table, and serialises everything that touches them (sync messages from
//! subscribers, server-side edits, reads, versions). Updates are stored one row each as they arrive and
//! compacted into the snapshot every so often; the indexes are written on a debounce. A replica with no
//! subscribers stops after a while, writing what is left.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use rookery_proto::RpcError;
use rookery_proto::documents::{DocumentAuthor, DocumentEditOp};
use rookery_store::documents::{DocumentIndex, Version};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use uuid::Uuid;
use yrs::encoding::read::Cursor;
use yrs::sync::awareness::{AwarenessUpdate, AwarenessUpdateEntry};
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::{Decode, DecoderV1};
use yrs::updates::encoder::Encode;
use yrs::{
    ClientID, Doc, OffsetKind, Options, ReadTxn, StateVector, Transact, Update, XmlFragment,
};

use super::schema::{self, FRAGMENT};
use crate::IntoRpc;
use rookery_kernel::Kernel;

/// Timings, adjustable by tests through [`set_tuning`].
#[derive(Debug, Clone)]
pub struct Tuning {
    /// Indexes are written this long after the last change...
    pub index_delay: Duration,
    /// ...but at least this often while changes keep coming.
    pub index_max: Duration,
    /// Updates stored after the snapshot before they are compacted into it.
    pub compact_after: usize,
    /// A replica nobody is subscribed to stops after this long.
    pub idle: Duration,
    /// A periodic version is saved when the last one is older than this and the document changed.
    pub version_every: Duration,
    /// How long a server-side author's presence stays after their edit.
    pub presence: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            index_delay: Duration::from_millis(800),
            index_max: Duration::from_secs(4),
            compact_after: 200,
            idle: Duration::from_secs(30),
            version_every: Duration::from_secs(600),
            presence: Duration::from_secs(15),
        }
    }
}

#[derive(Default)]
struct TuningCell(RwLock<Tuning>);

/// Replaces the timings (tests).
pub fn set_tuning(kernel: &Kernel, t: Tuning) {
    *kernel.facility::<TuningCell>().0.write().unwrap() = t;
}

fn tuning(kernel: &Kernel) -> Tuning {
    kernel.facility::<TuningCell>().0.read().unwrap().clone()
}

/// Who does something to a document.
#[derive(Debug, Clone)]
pub struct Who {
    /// The principal, as recorded with updates and versions.
    pub actor: String,
    pub user: Option<Uuid>,
    /// Shown in notifications ("Ada mentioned you in …").
    pub name: String,
}

pub enum Cmd {
    Join {
        sub: u64,
        tx: mpsc::UnboundedSender<Vec<u8>>,
        write: bool,
        who: Who,
    },
    Message {
        sub: u64,
        data: Vec<u8>,
    },
    Leave {
        sub: u64,
    },
    Read {
        reply: oneshot::Sender<Result<Value, RpcError>>,
    },
    /// The full state (update format v1).
    State {
        reply: oneshot::Sender<Result<Vec<u8>, RpcError>>,
    },
    Edit {
        ops: Vec<DocumentEditOp>,
        author: Option<DocumentAuthor>,
        who: Who,
        reply: oneshot::Sender<Result<u32, RpcError>>,
    },
    SaveVersion {
        kind: &'static str,
        name: Option<String>,
        who: Who,
        reply: oneshot::Sender<Result<Version, RpcError>>,
    },
    Restore {
        version: Uuid,
        who: Who,
        reply: oneshot::Sender<Result<Version, RpcError>>,
    },
    /// Writes the indexes and compacts now.
    Flush {
        reply: oneshot::Sender<Result<(), RpcError>>,
    },
    /// The document was deleted: drop everyone and stop.
    Close,
}

impl Cmd {
    /// Answers a command that cannot be served.
    fn fail(self, e: RpcError) {
        match self {
            Cmd::Read { reply } => drop(reply.send(Err(e))),
            Cmd::State { reply } => drop(reply.send(Err(e))),
            Cmd::Edit { reply, .. } => drop(reply.send(Err(e))),
            Cmd::SaveVersion { reply, .. } | Cmd::Restore { reply, .. } => drop(reply.send(Err(e))),
            Cmd::Flush { reply } => drop(reply.send(Err(e))),
            Cmd::Join { .. } | Cmd::Message { .. } | Cmd::Leave { .. } | Cmd::Close => {}
        }
    }
}

#[derive(Default)]
struct Registry {
    docs: Mutex<HashMap<Uuid, mpsc::UnboundedSender<Cmd>>>,
}

static NEXT_SUB: AtomicU64 = AtomicU64::new(1);

pub fn next_sub() -> u64 {
    NEXT_SUB.fetch_add(1, Ordering::Relaxed)
}

/// Sends a command to the document's replica, starting one if none runs.
pub fn send(kernel: &Kernel, id: Uuid, mut cmd: Cmd) {
    let reg = kernel.facility::<Registry>();
    loop {
        let tx = {
            let mut docs = reg.docs.lock().unwrap();
            docs.entry(id)
                .or_insert_with(|| {
                    let (tx, rx) = mpsc::unbounded_channel();
                    tokio::spawn(run(kernel.clone(), id, rx));
                    tx
                })
                .clone()
        };
        match tx.send(cmd) {
            Ok(()) => return,
            Err(mpsc::error::SendError(c)) => {
                // That replica stopped between the lookup and the send: forget it and start another.
                cmd = c;
                let mut docs = reg.docs.lock().unwrap();
                if docs.get(&id).is_some_and(|t| t.same_channel(&tx)) {
                    docs.remove(&id);
                }
            }
        }
    }
}

/// Sends a command if the document has a running replica.
pub fn send_if_live(kernel: &Kernel, id: Uuid, cmd: Cmd) {
    let reg = kernel.facility::<Registry>();
    if let Some(tx) = reg.docs.lock().unwrap().get(&id) {
        let _ = tx.send(cmd);
    }
}

/// Whether the document has a running replica (tests).
pub fn is_live(kernel: &Kernel, id: Uuid) -> bool {
    kernel
        .facility::<Registry>()
        .docs
        .lock()
        .unwrap()
        .contains_key(&id)
}

/// A request to the replica that waits for its answer.
pub async fn ask<T>(
    kernel: &Kernel,
    id: Uuid,
    make: impl FnOnce(oneshot::Sender<Result<T, RpcError>>) -> Cmd,
) -> Result<T, RpcError> {
    let (tx, rx) = oneshot::channel();
    send(kernel, id, make(tx));
    rx.await
        .map_err(|_| RpcError::unavailable("the document stopped"))?
}

/// A document from updates merged into one first: applying them one by one can trip yrs over gaps that the
/// merge resolves.
pub fn rebuild(parts: &[&[u8]]) -> Result<Doc, String> {
    let doc = new_doc();
    if !parts.is_empty() {
        let merged = yrs::merge_updates_v1(parts.iter().copied()).map_err(|e| e.to_string())?;
        let u = Update::decode_v1(&merged).map_err(|e| e.to_string())?;
        doc.transact_mut()
            .apply_update(u)
            .map_err(|e| e.to_string())?;
    }
    doc.get_or_insert_xml_fragment(FRAGMENT);
    Ok(doc)
}

/// What a document holds that waits for missing structs (encoded), or `None`.
fn pending(txn: &impl ReadTxn) -> Option<Vec<u8>> {
    txn.has_missing_updates()
        .then(|| txn.encode_state_as_update_v1(&txn.state_vector()))
}

/// Whether an update carries structs of a client beyond a gap: clocks of that client the document (with
/// state vector `sv`) does not have and the update does not bring either. yrs 0.28 integrates such a
/// struct at once when its neighbours are there, with a placeholder for the gap, and counts the gap in
/// its state vector as if it had it: the missing structs are then never asked for in a sync (the
/// replica and the clients diverge for good), and when they do come, filling the placeholder can panic
/// or place them differently than a replica that got them in order. Yjs keeps such structs pending.
/// They arrive this way when a client sends an edit before answering the kernel's sync step 1 after a
/// reconnect (the edit follows its offline edits, which come with that answer), and in updates that
/// carry a sender's own pending structs.
pub fn ahead_of(update: &Update, sv: &StateVector) -> bool {
    for (client, ranges) in update.insertions(true).iter() {
        let mut ranges: Vec<std::ops::Range<u32>> = ranges.iter().cloned().collect();
        ranges.sort_by_key(|r| r.start);
        let mut reach = sv.get(client);
        for r in ranges {
            if r.start > reach {
                return true;
            }
            reach = reach.max(r.end);
        }
    }
    false
}

pub fn new_doc() -> Doc {
    Doc::with_options(Options {
        offset_kind: OffsetKind::Utf16,
        ..Default::default()
    })
}

fn doc_with_client(client: ClientID) -> Doc {
    Doc::with_options(Options {
        client_id: client,
        offset_kind: OffsetKind::Utf16,
        ..Default::default()
    })
}

/// A stable Yjs client id for a server-side author, so their edits and presence keep one identity.
fn client_for(actor: &str) -> ClientID {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    actor.hash(&mut h);
    ClientID::new((h.finish() & 0x3fff_ffff) | 0x4000_0000)
}

/// A CSS colour from a name, one of the label tokens.
pub fn colour_for(name: &str) -> String {
    const LABELS: &[&str] = &["sky", "teal", "amber", "rose", "mint", "orchid", "ember"];
    let sum: usize = name.bytes().map(usize::from).sum();
    format!("var(--label-{})", LABELS[sum % LABELS.len()])
}

/// One message in y-protocols encoding.
fn encode(msg: &Message) -> Vec<u8> {
    msg.encode_v1()
}

struct Sub {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    write: bool,
    who: Who,
    /// Awareness client ids this subscriber announced, removed when it leaves.
    clients: HashSet<u64>,
    told_read_only: bool,
}

/// Presence as y-protocols' awareness keeps it: per client id a clock and a JSON state.
#[derive(Default)]
struct Presence {
    states: HashMap<u64, (u32, Arc<str>)>,
}

impl Presence {
    /// Applies an update with the y-protocols rules; returns whether anything changed.
    fn apply(&mut self, u: &AwarenessUpdate) -> bool {
        let mut changed = false;
        for (id, e) in &u.clients {
            let id = id.get();
            let null = e.json.as_ref() == "null";
            match self.states.get(&id) {
                Some((clock, _)) if *clock > e.clock => {}
                Some((clock, _)) if *clock == e.clock && !null => {}
                _ => {
                    if null {
                        self.states.remove(&id);
                    } else {
                        self.states.insert(id, (e.clock, e.json.clone()));
                    }
                    changed = true;
                }
            }
        }
        changed
    }

    fn full(&self) -> Option<AwarenessUpdate> {
        if self.states.is_empty() {
            return None;
        }
        Some(AwarenessUpdate {
            clients: self
                .states
                .iter()
                .map(|(id, (clock, json))| {
                    (
                        ClientID::new(*id),
                        AwarenessUpdateEntry {
                            clock: *clock,
                            json: json.clone(),
                        },
                    )
                })
                .collect(),
        })
    }

    /// Removes clients and returns the update that tells the others: a `null` state with the same
    /// clock, which y-protocols accepts as a removal and which a returning client's next clock beats.
    fn remove(&mut self, ids: impl IntoIterator<Item = u64>) -> Option<AwarenessUpdate> {
        let mut clients = HashMap::new();
        for id in ids {
            if let Some((clock, _)) = self.states.remove(&id) {
                clients.insert(
                    ClientID::new(id),
                    AwarenessUpdateEntry {
                        clock,
                        json: "null".into(),
                    },
                );
            }
        }
        (!clients.is_empty()).then_some(AwarenessUpdate { clients })
    }
}

struct Replica {
    kernel: Kernel,
    id: Uuid,
    doc: Doc,
    tuning: Tuning,
    subs: HashMap<u64, Sub>,
    presence: Presence,
    /// Server-side authors' presence and when it ends.
    authors: HashMap<u64, Instant>,
    next_seq: i64,
    /// Updates stored after the snapshot.
    pending: usize,
    /// A change since the indexes were written: since when, and the latest.
    dirty: Option<(Instant, Instant)>,
    changed_since_version: bool,
    last_version: Option<Instant>,
    /// The last one to change the document, named in mention notifications.
    last_editor: Option<Who>,
    last_activity: Instant,
    /// Updates that arrived ahead of structs they follow (see [`ahead_of`]), with who sent them, kept
    /// until what they follow has come.
    held: Vec<Held>,
}

struct Held {
    data: Vec<u8>,
    from: Option<u64>,
    who: Who,
}

/// The most updates kept waiting for what they follow; past it the oldest is dropped (its sender sends
/// it again when it reconnects, as its sync step 2 answers the kernel's state vector).
const MAX_HELD: usize = 1000;

async fn run(kernel: Kernel, id: Uuid, mut rx: mpsc::UnboundedReceiver<Cmd>) {
    let mut r = match Replica::load(kernel.clone(), id).await {
        Ok(r) => r,
        Err(e) => {
            // Nothing can be served: answer what is queued, then let `send` start afresh next time.
            let reg = kernel.facility::<Registry>();
            reg.docs.lock().unwrap().remove(&id);
            rx.close();
            while let Ok(c) = rx.try_recv() {
                c.fail(e.clone());
            }
            return;
        }
    };
    loop {
        let wake = r.next_wake();
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break };
                if r.handle(cmd).await.is_break() {
                    break;
                }
            }
            _ = tokio::time::sleep_until(wake) => {
                r.timers().await;
                if r.subs.is_empty() && r.last_activity.elapsed() >= r.tuning.idle {
                    // Stop, unless a command slipped in: the registry lock orders this against `send`.
                    let reg = kernel.facility::<Registry>();
                    let mut docs = reg.docs.lock().unwrap();
                    if rx.is_empty() {
                        docs.remove(&id);
                        rx.close();
                        break;
                    }
                }
            }
            _ = kernel.shutdown.cancelled() => break,
        }
    }
    r.finish().await;
    // Anything that arrived after the decision to stop is served by a new replica.
    while let Ok(c) = rx.try_recv() {
        match c {
            Cmd::Close | Cmd::Leave { .. } | Cmd::Message { .. } => {}
            c => send(&kernel, id, c),
        }
    }
    let reg = kernel.facility::<Registry>();
    let mut docs = reg.docs.lock().unwrap();
    if docs.get(&id).is_some_and(|t| t.is_closed()) {
        docs.remove(&id);
    }
}

impl Replica {
    async fn load(kernel: Kernel, id: Uuid) -> Result<Replica, RpcError> {
        let stored = kernel.store.documents().load(id).await.rpc()?;
        let mut parts: Vec<&[u8]> = Vec::new();
        if let Some(s) = &stored.snapshot {
            parts.push(s);
        }
        parts.extend(stored.updates.iter().map(|(_, u)| u.as_slice()));
        let doc = rebuild(&parts).map_err(|e| RpcError::internal(format!("document {id}: {e}")))?;
        doc.get_or_insert_xml_fragment(FRAGMENT);
        let last_version = kernel
            .store
            .documents()
            .last_version_at(id)
            .await
            .rpc()?
            .map(|at| {
                let age = (chrono::Utc::now() - at).to_std().unwrap_or_default();
                Instant::now().checked_sub(age).unwrap_or_else(Instant::now)
            });
        Ok(Replica {
            tuning: tuning(&kernel),
            kernel,
            id,
            doc,
            subs: HashMap::new(),
            presence: Presence::default(),
            authors: HashMap::new(),
            next_seq: stored.next_seq(),
            pending: stored.updates.len(),
            dirty: None,
            changed_since_version: false,
            last_version,
            last_editor: None,
            last_activity: Instant::now(),
            held: Vec::new(),
        })
    }

    fn next_wake(&self) -> Instant {
        let far = Instant::now() + Duration::from_secs(3600);
        let mut wake = far;
        if let Some((first, last)) = self.dirty {
            wake = wake.min((last + self.tuning.index_delay).min(first + self.tuning.index_max));
        }
        if self.subs.is_empty() {
            wake = wake.min(self.last_activity + self.tuning.idle);
        }
        for at in self.authors.values() {
            wake = wake.min(*at);
        }
        wake
    }

    async fn timers(&mut self) {
        let now = Instant::now();
        if let Some((first, last)) = self.dirty
            && (now >= last + self.tuning.index_delay || now >= first + self.tuning.index_max)
        {
            self.index().await;
        }
        let gone: Vec<u64> = self
            .authors
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(c, _)| *c)
            .collect();
        if !gone.is_empty() {
            for c in &gone {
                self.authors.remove(c);
            }
            if let Some(u) = self.presence.remove(gone) {
                self.broadcast(&encode(&Message::Awareness(u)), None);
            }
        }
    }

    async fn handle(&mut self, cmd: Cmd) -> std::ops::ControlFlow<()> {
        self.last_activity = Instant::now();
        match cmd {
            Cmd::Join {
                sub,
                tx,
                write,
                who,
            } => {
                let sv = self.doc.transact().state_vector();
                let _ = tx.send(encode(&Message::Sync(SyncMessage::SyncStep1(sv))));
                if let Some(u) = self.presence.full() {
                    let _ = tx.send(encode(&Message::Awareness(u)));
                }
                self.subs.insert(
                    sub,
                    Sub {
                        tx,
                        write,
                        who,
                        clients: HashSet::new(),
                        told_read_only: false,
                    },
                );
            }
            Cmd::Message { sub, data } => {
                if let Err(e) = self.message(sub, &data).await {
                    tracing::debug!(document = %self.id, error = %e, "dropping a subscriber's bad message");
                    self.leave(sub);
                }
            }
            Cmd::Leave { sub } => self.leave(sub),
            Cmd::Read { reply } => {
                let txn = self.doc.transact();
                let frag = txn.get_xml_fragment(FRAGMENT).expect("created at load");
                let _ = reply.send(Ok(schema::to_json(&txn, &frag)));
            }
            Cmd::State { reply } => {
                let _ = reply.send(Ok(self.state()));
            }
            Cmd::Edit {
                ops,
                author,
                who,
                reply,
            } => {
                let r = self.edit(&ops, author, &who).await;
                let _ = reply.send(r);
            }
            Cmd::SaveVersion {
                kind,
                name,
                who,
                reply,
            } => {
                let r = self
                    .save_version(kind, name.as_deref(), Some(&who.actor))
                    .await;
                let _ = reply.send(r);
            }
            Cmd::Restore {
                version,
                who,
                reply,
            } => {
                let r = self.restore(version, &who).await;
                let _ = reply.send(r);
            }
            Cmd::Flush { reply } => {
                if self.dirty.is_some() {
                    self.index().await;
                }
                let r = self.compact().await;
                let _ = reply.send(r);
            }
            Cmd::Close => {
                self.subs.clear();
                self.dirty = None;
                self.changed_since_version = false;
                self.pending = 0;
                return std::ops::ControlFlow::Break(());
            }
        }
        std::ops::ControlFlow::Continue(())
    }

    fn state(&self) -> Vec<u8> {
        self.doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }

    fn send_to(&self, sub: u64, msg: &Message) {
        if let Some(s) = self.subs.get(&sub) {
            let _ = s.tx.send(encode(msg));
        }
    }

    fn broadcast(&self, bytes: &[u8], except: Option<u64>) {
        for (id, s) in &self.subs {
            if Some(*id) != except {
                let _ = s.tx.send(bytes.to_vec());
            }
        }
    }

    fn leave(&mut self, sub: u64) {
        if let Some(s) = self.subs.remove(&sub)
            && let Some(u) = self.presence.remove(s.clients)
        {
            self.broadcast(&encode(&Message::Awareness(u)), None);
        }
    }

    async fn message(&mut self, sub: u64, data: &[u8]) -> Result<(), RpcError> {
        let bad = |e: yrs::encoding::read::Error| RpcError::invalid(format!("bad message: {e}"));
        let mut dec = DecoderV1::new(Cursor::new(data));
        let msg = Message::decode(&mut dec).map_err(bad)?;
        match msg {
            Message::Sync(SyncMessage::SyncStep1(sv)) => {
                let diff = self.doc.transact().encode_state_as_update_v1(&sv);
                self.send_to(sub, &Message::Sync(SyncMessage::SyncStep2(diff)));
            }
            Message::Sync(SyncMessage::SyncStep2(u)) | Message::Sync(SyncMessage::Update(u)) => {
                let Some(s) = self.subs.get_mut(&sub) else {
                    return Ok(());
                };
                if !s.write {
                    if !s.told_read_only {
                        s.told_read_only = true;
                        let _ = s.tx.send(encode(&Message::Auth(Some("read-only".into()))));
                    }
                    return Ok(());
                }
                let who = s.who.clone();
                self.apply(&u, Some(sub), &who).await?;
            }
            Message::Awareness(u) => {
                if let Some(s) = self.subs.get_mut(&sub) {
                    for (id, e) in &u.clients {
                        if e.json.as_ref() == "null" {
                            s.clients.remove(&id.get());
                        } else {
                            s.clients.insert(id.get());
                        }
                    }
                }
                if self.presence.apply(&u) {
                    self.broadcast(&encode(&Message::Awareness(u)), Some(sub));
                }
            }
            Message::AwarenessQuery => {
                if let Some(u) = self.presence.full() {
                    self.send_to(sub, &Message::Awareness(u));
                }
            }
            Message::Auth(_) | Message::Custom(..) => {}
        }
        Ok(())
    }

    /// Applies an update: in memory, to the store, to everyone else. An update that is ahead of what the
    /// replica has waits until it is not, then goes the same way.
    async fn apply(&mut self, data: &[u8], from: Option<u64>, who: &Who) -> Result<bool, RpcError> {
        let update =
            Update::decode_v1(data).map_err(|e| RpcError::invalid(format!("bad update: {e}")))?;
        if ahead_of(&update, &self.doc.transact().state_vector()) {
            if self.held.len() >= MAX_HELD {
                tracing::warn!(document = %self.id, "too many updates wait for what they follow; dropping the oldest");
                self.held.remove(0);
            }
            self.held.push(Held {
                data: data.to_vec(),
                from,
                who: who.clone(),
            });
            return Ok(false);
        }
        let changed = self.integrate(update, data, from, who).await?;
        if changed && !self.held.is_empty() {
            self.release_held().await;
        }
        Ok(changed)
    }

    /// Applies the held updates that are no longer ahead, until none is.
    async fn release_held(&mut self) {
        loop {
            let sv = self.doc.transact().state_vector();
            let Some(i) = self
                .held
                .iter()
                .position(|h| Update::decode_v1(&h.data).is_ok_and(|u| !ahead_of(&u, &sv)))
            else {
                return;
            };
            let h = self.held.remove(i);
            let Ok(update) = Update::decode_v1(&h.data) else {
                continue;
            };
            if let Err(e) = self.integrate(update, &h.data, h.from, &h.who).await {
                tracing::debug!(document = %self.id, error = %e, "a held update could not be applied");
            }
        }
    }

    async fn integrate(
        &mut self,
        update: Update,
        data: &[u8],
        from: Option<u64>,
        who: &Who,
    ) -> Result<bool, RpcError> {
        let applied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut txn = self.doc.transact_mut();
            let pending_before = pending(&txn);
            txn.apply_update(update)
                .map_err(|e| RpcError::invalid(format!("bad update: {e}")))?;
            // An update whose structs wait for one that has not arrived yet changes no state vector, but
            // it is new all the same: it is stored and passed on, or the others never get those structs
            // (they integrate here once the missing one comes, and only that one would be passed on).
            Ok::<bool, RpcError>(
                txn.before_state() != txn.after_state()
                    || !txn.delete_set().is_empty()
                    || pending(&txn) != pending_before,
            )
        }));
        let changed = match applied {
            Ok(r) => r?,
            Err(_) => {
                // yrs 0.28 can panic integrating an update next to a gap ("skip") that an earlier update with
                // pending structs left in the block store. The same updates merged into one apply cleanly, so
                // the replica starts again from what is stored plus this update.
                tracing::warn!(document = %self.id, "applying an update panicked; rebuilding the replica");
                self.recover(data).await?;
                true
            }
        };
        if !changed {
            return Ok(false);
        }
        self.broadcast(
            &encode(&Message::Sync(SyncMessage::Update(data.to_vec()))),
            from,
        );
        let seq = self.next_seq;
        self.next_seq += 1;
        if let Err(e) = self
            .kernel
            .store
            .documents()
            .append(self.id, seq, data, Some(&who.actor))
            .await
        {
            // The update is in memory and goes into the next snapshot.
            tracing::warn!(document = %self.id, error = %e, "storing an update failed");
        }
        self.pending += 1;
        let now = Instant::now();
        self.dirty = Some(match self.dirty {
            Some((first, _)) => (first, now),
            None => (now, now),
        });
        self.changed_since_version = true;
        self.last_editor = Some(who.clone());
        if self.pending >= self.tuning.compact_after
            && let Err(e) = self.compact().await
        {
            tracing::warn!(document = %self.id, error = %e, "compacting failed");
        }
        Ok(true)
    }

    /// Rebuilds the document from the store and `data` (an update that could not be applied in place).
    async fn recover(&mut self, data: &[u8]) -> Result<(), RpcError> {
        let stored = self.kernel.store.documents().load(self.id).await.rpc()?;
        let mut parts: Vec<&[u8]> = Vec::new();
        if let Some(s) = &stored.snapshot {
            parts.push(s);
        }
        parts.extend(stored.updates.iter().map(|(_, u)| u.as_slice()));
        // Updates that are not stored yet (a failed append) live only in the old replica; take its state too
        // when it can still be read.
        let current = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.state())).ok();
        if let Some(c) = &current {
            parts.push(c);
        }
        parts.push(data);
        self.doc = rebuild(&parts)
            .map_err(|e| RpcError::internal(format!("document {}: {e}", self.id)))?;
        Ok(())
    }

    async fn compact(&mut self) -> Result<(), RpcError> {
        if self.pending == 0 {
            return Ok(());
        }
        let state = self.state();
        self.kernel
            .store
            .documents()
            .compact(self.id, &state, self.next_seq - 1)
            .await
            .rpc()?;
        self.pending = 0;
        Ok(())
    }

    /// Writes the title, search text and references, notifies newly mentioned users, and saves a
    /// periodic version when one is due.
    async fn index(&mut self) {
        self.dirty = None;
        let content = {
            let txn = self.doc.transact();
            let frag = txn.get_xml_fragment(FRAGMENT).expect("created at load");
            schema::to_json(&txn, &frag)
        };
        if let Err(e) =
            index_content(&self.kernel, self.id, &content, self.last_editor.as_ref()).await
        {
            tracing::warn!(document = %self.id, error = %e, "indexing failed");
        }
        let due = self
            .last_version
            .is_none_or(|at| at.elapsed() >= self.tuning.version_every);
        if self.changed_since_version
            && due
            && let Err(e) = self.save_version("auto", None, None).await
        {
            tracing::warn!(document = %self.id, error = %e, "saving a version failed");
        }
    }

    async fn save_version(
        &mut self,
        kind: &'static str,
        name: Option<&str>,
        by: Option<&str>,
    ) -> Result<Version, RpcError> {
        let by = by.or(self.last_editor.as_ref().map(|w| w.actor.as_str()));
        let v = self
            .kernel
            .store
            .documents()
            .add_version(self.id, kind, name, &self.state(), by)
            .await
            .rpc()?;
        self.last_version = Some(Instant::now());
        self.changed_since_version = false;
        Ok(v)
    }

    async fn edit(
        &mut self,
        ops: &[DocumentEditOp],
        author: Option<DocumentAuthor>,
        who: &Who,
    ) -> Result<u32, RpcError> {
        let client = client_for(&who.actor);
        // Edit as a participant of its own: a copy of the document with the author's client id.
        let mine = doc_with_client(client);
        {
            let mut txn = mine.transact_mut();
            let u =
                Update::decode_v1(&self.state()).map_err(|e| RpcError::internal(e.to_string()))?;
            txn.apply_update(u)
                .map_err(|e| RpcError::internal(e.to_string()))?;
        }
        let frag = mine.get_or_insert_xml_fragment(FRAGMENT);
        let blocks = {
            let mut txn = mine.transact_mut();
            super::edit::apply(&mut txn, &frag, ops)?;
            frag.len(&txn)
        };
        let sv = self.doc.transact().state_vector();
        let diff = mine.transact().encode_state_as_update_v1(&sv);
        self.apply(&diff, None, who).await?;
        // Show who edited to the people in the document.
        let name = author
            .as_ref()
            .map(|a| a.name.clone())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| who.name.clone());
        let colour = author
            .and_then(|a| a.color)
            .unwrap_or_else(|| colour_for(&name));
        let clock = self
            .presence
            .states
            .get(&client.get())
            .map(|(c, _)| c + 1)
            .unwrap_or(1);
        let state =
            json!({ "user": { "name": name, "color": colour, "kind": "server" } }).to_string();
        let u = AwarenessUpdate {
            clients: HashMap::from([(
                client,
                AwarenessUpdateEntry {
                    clock,
                    json: state.into(),
                },
            )]),
        };
        if self.presence.apply(&u) {
            self.broadcast(&encode(&Message::Awareness(u)), None);
        }
        self.authors
            .insert(client.get(), Instant::now() + self.tuning.presence);
        Ok(blocks)
    }

    async fn restore(&mut self, version: Uuid, who: &Who) -> Result<Version, RpcError> {
        let (_, data) = self
            .kernel
            .store
            .documents()
            .version(self.id, version)
            .await
            .rpc()?;
        let old = new_doc();
        {
            let mut txn = old.transact_mut();
            let u = Update::decode_v1(&data).map_err(|e| RpcError::internal(e.to_string()))?;
            txn.apply_update(u)
                .map_err(|e| RpcError::internal(e.to_string()))?;
        }
        let frag = old.get_or_insert_xml_fragment(FRAGMENT);
        let blocks = schema::blocks_json(&old.transact(), &frag);
        let before = self.save_version("restore", None, Some(&who.actor)).await?;
        self.edit(
            &[DocumentEditOp::SetContent {
                content: Some(Value::Array(blocks)),
                markdown: None,
            }],
            None,
            who,
        )
        .await?;
        Ok(before)
    }

    /// Writes what is left before the replica stops.
    async fn finish(&mut self) {
        if self.dirty.is_some() {
            self.index().await;
        }
        if self.changed_since_version
            && let Err(e) = self.save_version("auto", None, None).await
        {
            tracing::warn!(document = %self.id, error = %e, "saving a version failed");
        }
        if let Err(e) = self.compact().await {
            tracing::warn!(document = %self.id, error = %e, "compacting failed");
        }
    }
}

/// A block a plugin contributes to documents (`contributes.blocks`): its id, and the templates of its
/// plain text and Markdown, filled from the block's attributes.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockTemplate {
    pub id: String,
    pub text: Option<String>,
    pub markdown: Option<String>,
}

/// The blocks of each plugin, which the plugin system sets with [`set_blocks`] when it loads one.
#[derive(Default)]
struct Blocks(std::sync::RwLock<HashMap<String, Vec<BlockTemplate>>>);

/// Sets the blocks a plugin contributes, replacing those it had.
pub fn set_blocks(kernel: &Kernel, plugin: &str, blocks: Vec<BlockTemplate>) {
    let all = kernel.facility::<Blocks>();
    let mut all = all.0.write().unwrap();
    if blocks.is_empty() {
        all.remove(plugin);
    } else {
        all.insert(plugin.to_owned(), blocks);
    }
}

/// Plain text and Markdown for plugin blocks, from the contributions of the plugins that are loaded.
pub struct BlockTemplates(HashMap<String, (Option<String>, Option<String>)>);

impl BlockTemplates {
    pub fn of(kernel: &Kernel) -> BlockTemplates {
        let all = kernel.facility::<Blocks>();
        let mut map = HashMap::new();
        for blocks in all.0.read().unwrap().values() {
            for b in blocks {
                map.insert(b.id.clone(), (b.text.clone(), b.markdown.clone()));
            }
        }
        BlockTemplates(map)
    }

    fn attrs(node: &Value) -> (&str, &Value) {
        let a = node.get("attrs");
        let ty = a
            .and_then(|a| a.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let attrs = a.and_then(|a| a.get("attrs")).unwrap_or(&Value::Null);
        (ty, attrs)
    }

    pub fn text(&self, node: &Value) -> Option<String> {
        let (ty, attrs) = Self::attrs(node);
        let (t, _) = self.0.get(ty)?;
        Some(schema::fill_template(t.as_deref()?, attrs))
    }

    pub fn markdown(&self, node: &Value) -> Option<String> {
        let (ty, attrs) = Self::attrs(node);
        let (t, md) = self.0.get(ty)?;
        Some(schema::fill_template(
            md.as_deref().or(t.as_deref())?,
            attrs,
        ))
    }
}

/// Puts a document's content into the indexes and notifies users mentioned for the first time.
pub async fn index_content(
    kernel: &Kernel,
    id: Uuid,
    content: &Value,
    editor: Option<&Who>,
) -> Result<(), RpcError> {
    let templates = BlockTemplates::of(kernel);
    let bt = |n: &Value| templates.text(n);
    let blocks = content
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let ix = DocumentIndex {
        title: schema::title_of(blocks, &bt),
        text: schema::plain_text_of(content, &bt),
        mentions: schema::references(content),
    };
    let store = &kernel.store;
    let mentioned = store.documents().index(id, &ix).await.rpc()?;
    // Mentions notify when a person (or an agent acting for one) wrote them; a plugin acting as itself, such
    // as a migration moving old content into documents, notifies nobody.
    if mentioned.is_empty() || editor.is_none_or(|w| w.user.is_none()) {
        return Ok(());
    }
    let Some(doc) = store.documents().get(id).await.rpc()? else {
        return Ok(());
    };
    let source = doc.source();
    let place = match store
        .entities()
        .get(&rookery_store::Caller::system(), &source)
        .await
    {
        Ok(Some(e)) => e.title,
        _ => doc.title.clone(),
    };
    let who = editor
        .map(|w| w.name.clone())
        .unwrap_or_else(|| "Someone".into());
    for user in mentioned {
        if editor.and_then(|w| w.user) == Some(user) {
            continue;
        }
        let data = json!({
            "entity": source,
            "plugin": doc.plugin,
            "document": id.to_string(),
            "field": rookery_store::documents::ref_field(id),
        });
        let title = format!("{who} mentioned you in {place}");
        let actor = editor
            .map(|w| w.actor.clone())
            .unwrap_or_else(|| "system:rookery".into());
        let sent = store
            .notify()
            .send(rookery_store::notify::New {
                user,
                kind: "rookery/mention",
                title: &title,
                body: None,
                data: Some(&data),
                actions: None,
                sent_by: &actor,
            })
            .await;
        match sent {
            Ok(Some(n)) => kernel.conns.notify_user(
                &user.to_string(),
                "notify.new",
                rookery_jobs::notify::info(n),
            ),
            Ok(None) => {}
            Err(e) => tracing::warn!(document = %id, error = %e, "a mention's notification failed"),
        }
    }
    Ok(())
}
