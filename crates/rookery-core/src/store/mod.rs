//! What rookery keeps: messages taken in from the hosts' channels, a small
//! key-value store, and state of kinds. `rookery serve` keeps it in its tables
//! on Postgres, through a [`Backend`] of rookery-hosts; tests keep it in memory
//! ([`memory`]).

mod installation;
#[cfg(any(test, feature = "testing"))]
pub mod memory;

pub use installation::{hold_lock, set_shared, shared, wait_lock};

use rookery_api::{Message, ids, rfc3339, unix_now};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

/// The kinds of values in the `state` table.
pub mod kind {
    pub const PROFILE: &str = "profile";
    pub const CREDENTIALS: &str = "credentials";
    /// What a provisioner made for rookery, by its id.
    pub const RESOURCE: &str = "resource";
    /// A resource's specification, by the resource's id.
    pub const SPEC: &str = "spec";
    /// The values of a provisioner's own arguments, by `PLUGIN/ID`.
    pub const PROVISIONER: &str = "provisioner";
    /// A provisioner rookery runs under a name of its own: its type and settings.
    pub const PROVISIONER_ENTRY: &str = "provisioner-entry";
    pub const SETTING: &str = "setting";
    pub const REGISTRY: &str = "registry";
    pub const SECRET: &str = "secret";
    pub const KEY: &str = "key";
    /// What rookery knows of a host's SSH: its host keys and the keys it trusts.
    pub const ACCESS: &str = "access";
    /// What rookery knows of a host's device on the tailnet, and a new name
    /// it is to take.
    pub const TAILNET: &str = "tailnet";
    pub const TOKEN: &str = "token";
    pub const PLUGIN: &str = "plugin";
    pub const MIRROR: &str = "mirror";
    pub const META: &str = "meta";
}

/// What people call a thing of a kind of `state`.
pub fn noun(kind: &str) -> &str {
    match id_kind(kind) {
        Some(kind) => kind.noun(),
        None => kind,
    }
}

/// The kind of id the things of a kind of `state` have, if they have one.
pub fn id_kind(kind: &str) -> Option<ids::Kind> {
    Some(match kind {
        kind::PROFILE => ids::Kind::Host,
        kind::PROVISIONER_ENTRY => ids::Kind::Provisioner,
        kind::RESOURCE => ids::Kind::Resource,
        kind::REGISTRY => ids::Kind::Registry,
        kind::SECRET => ids::Kind::Secret,
        kind::KEY => ids::Kind::Key,
        _ => return None,
    })
}

/// A new id: a UUID of version 7 as [`rookery_api::ids`] writes it.
pub fn new_id() -> String {
    use std::io::Read;
    let mut random = [0u8; 10];
    // The kernel's random source does not fail once the system has booted.
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut random))
        .expect("reading /dev/urandom");
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default();
    ids::from_parts(millis, random)
}

/// Whether a text may be the label of a thing: not empty, and not in the
/// shape of an id.
pub fn check_label(kind: &str, label: &str) -> Result<()> {
    if label.trim().is_empty() {
        return Err(Error::Given(format!("A {} needs a name", noun(kind))));
    }
    if ids::looks_like_id(label) {
        return Err(Error::Given(format!(
            "{label} is in the shape of an id, which no name of a {} may be",
            noun(kind)
        )));
    }
    Ok(())
}

/// A thing found by its name or id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub id: String,
    pub name: String,
}

/// The scope of this installation's own state; a mirror of a rookery server uses another.
pub const OWN: &str = "";

/// A value's size limit.
pub const VALUE_LIMIT: usize = 64 * 1024;
/// How long messages and jobs are kept unless the settings say otherwise.
pub const RETENTION_DAYS: u64 = 90;

#[derive(Debug)]
pub enum Error {
    /// The condition of a conditional write did not hold. Carries the
    /// revision that is there, if the key exists.
    Conflict(Option<i64>),
    TooLarge,
    Database(String),
    /// A value is sealed with a key rookery does not have. Names the key file
    /// it looked for and the id of the key, never key material.
    Encrypted {
        key_file: Option<PathBuf>,
        key_id: Option<String>,
    },
    /// A sealed value that does not open with its key, or a key that cannot be used.
    Sealing(String),
    /// Another thing of the kind has the label already.
    Taken {
        kind: String,
        label: String,
    },
    /// A value given for a thing that cannot be one, such as the id of
    /// another kind.
    Given(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Conflict(Some(revision)) => write!(f, "the key is at revision {revision}"),
            Error::Conflict(None) => f.write_str("the key does not exist"),
            Error::TooLarge => write!(f, "a value is at most {VALUE_LIMIT} bytes"),
            Error::Database(message) => f.write_str(message),
            Error::Encrypted { key_file, key_id } => {
                f.write_str("the store is encrypted")?;
                if let Some(id) = key_id {
                    write!(f, " with key {id}")?;
                }
                match key_file {
                    Some(file) => write!(
                        f,
                        ", and rookery has no key that opens it: it looked in {} and beside it",
                        file.display()
                    ),
                    None => f.write_str(", and rookery was given no key that opens it"),
                }
            }
            Error::Sealing(message) => f.write_str(message),
            Error::Taken { kind, label } => write!(f, "There is a {} {label} already", noun(kind)),
            Error::Given(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// When a write to the key-value store may happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    Always,
    /// Only if the key is at this revision.
    At(i64),
    /// Only if the key does not exist.
    New,
}

#[derive(Debug, Clone)]
pub struct Stored {
    /// The order it was taken in; what `before` and `next` count in.
    pub seq: i64,
    pub host: String,
    pub message: Message,
    pub done: bool,
}

impl Stored {
    pub fn json(&self) -> Value {
        let mut value = serde_json::to_value(&self.message).unwrap_or_default();
        value["host"] = json!(self.host);
        value["done"] = json!(self.done);
        value["seq"] = json!(self.seq);
        value
    }
}

#[derive(Debug, Clone, Default)]
pub struct Filter<'a> {
    pub host: Option<&'a str>,
    /// Topics starting with this.
    pub topic: Option<&'a str>,
    /// Only messages taken in before this `seq`.
    pub before: Option<i64>,
    pub done: Option<bool>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub key: String,
    pub value: Value,
    pub revision: i64,
    pub updated: String,
}

/// A row of the `state` table: a value of some kind, for a scope, by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRow {
    pub kind: String,
    pub scope: String,
    pub name: String,
    pub value: Vec<u8>,
    pub sealed: bool,
    pub updated: String,
    /// The name of a thing with an id, which `name` holds; none for any
    /// other row. Unique within its kind and scope.
    pub label: Option<String>,
}

/// A change of `state` and of the hosts and subjects other tables name,
/// made in one transaction in this order: the removals, the rows of owners,
/// the writes, then the moves of messages, cursors and jobs.
#[derive(Debug, Clone, Default)]
pub struct Change {
    /// (kind, scope, name)
    pub removals: Vec<(String, String, String)>,
    /// (scope, owner id): the rows that belong to the owner, as
    /// [`owned_by`] tells them.
    pub owners: Vec<(String, String)>,
    pub writes: Vec<StateRow>,
    /// (old, new) host of messages and cursors.
    pub hosts: Vec<(String, String)>,
    /// (job id, host, subject) to set.
    pub jobs: Vec<(String, String, String)>,
}

/// Whether a row without a label belongs to the thing with the id `owner`:
/// its name is the id, or starts with the id and a `/`.
pub fn owned_by(row_name: &str, owner: &str) -> bool {
    row_name == owner
        || row_name
            .strip_prefix(owner)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// What a database has to do for the store. Times are given by `Store`, so
/// every backend writes the same.
pub trait Backend: Send {
    fn name(&self) -> &'static str;
    /// Whether it seals secrets and keys where it keeps them.
    fn seals(&self) -> bool {
        false
    }

    fn cursor(&mut self, host: &str) -> Result<u64>;
    fn take_in(&mut self, host: &str, message: &Message) -> Result<Option<Stored>>;
    fn reset_cursor(&mut self, host: &str) -> Result<()>;
    /// The messages that pass the filter, newest first, at most `limit` of
    /// them; `filter.limit` is left to `Store`.
    fn inbox(&mut self, filter: &Filter<'_>, limit: usize) -> Result<Vec<Stored>>;
    fn set_done(&mut self, host: &str, id: u64, done: bool) -> Result<bool>;

    fn get(&mut self, namespace: &str, key: &str) -> Result<Option<Entry>>;
    /// Write `text`, a JSON value, if `when` holds. Returns the new revision.
    fn put(
        &mut self,
        namespace: &str,
        key: &str,
        text: &str,
        when: When,
        updated: &str,
    ) -> Result<i64>;
    fn remove(&mut self, namespace: &str, key: &str, when: When) -> Result<bool>;
    fn list(&mut self, namespace: &str) -> Result<Vec<Entry>>;

    /// Drop messages from before `cutoff` and jobs that ended and started
    /// before it, in one transaction. Returns the dropped jobs' output files.
    fn prune(&mut self, cutoff: &str) -> Result<Vec<String>>;

    fn state(&mut self, kind: &str, scope: &str, name: &str) -> Result<Option<StateRow>>;
    /// A scope's rows of a kind, by name.
    fn states(&mut self, kind: &str, scope: &str) -> Result<Vec<StateRow>>;
    /// Insert the row or replace the one with its key.
    fn put_state(&mut self, row: &StateRow) -> Result<()>;
    fn remove_state(&mut self, kind: &str, scope: &str, name: &str) -> Result<bool>;
    /// In one transaction: delete every row of `scope` whose kind is in
    /// `kinds`, then insert `rows`.
    fn replace_states(&mut self, scope: &str, kinds: &[&str], rows: &[StateRow]) -> Result<()>;
    /// Every row of the table.
    fn all_states(&mut self) -> Result<Vec<StateRow>>;
    /// The row of a kind and scope with this label.
    fn labelled(&mut self, kind: &str, scope: &str, label: &str) -> Result<Option<StateRow>>;
    /// Give a row another label. False when there is no such row.
    fn relabel(
        &mut self,
        kind: &str,
        scope: &str,
        name: &str,
        label: &str,
        updated: &str,
    ) -> Result<bool>;
    /// Make a change in one transaction, in the order [`Change`] lists.
    fn change_states(&mut self, change: &Change) -> Result<()>;
}

pub struct Store {
    backend: Mutex<Box<dyn Backend>>,
}

fn now() -> String {
    rfc3339(unix_now())
}

impl Store {
    /// A store on a backend of the caller's, such as the tables of `rookery serve` on Postgres.
    pub fn from_backend(backend: Box<dyn Backend>) -> Store {
        Store::new(backend)
    }

    fn new(backend: Box<dyn Backend>) -> Store {
        Store {
            backend: Mutex::new(backend),
        }
    }

    /// A store in memory, for tests.
    #[cfg(any(test, feature = "testing"))]
    pub fn memory() -> Store {
        Store::new(Box::new(memory::MemoryBackend::default()))
    }

    // A backend that panicked part-way holds no half-done transaction, as
    // each runs in one call, so the lock is taken back.
    fn backend(&self) -> MutexGuard<'_, Box<dyn Backend>> {
        self.backend
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// "sqlite" or "postgres".
    pub fn backend_name(&self) -> &'static str {
        self.backend().name()
    }

    // ── Messages ────────────────────────────────────────────────────────────

    /// The last message id taken in from a host; 0 before the first.
    pub fn cursor(&self, host: &str) -> Result<u64> {
        self.backend().cursor(host)
    }

    /// Take in a message from a host's channel and move the host's cursor, in
    /// one transaction. A `none` message only moves the cursor. Returns what
    /// was stored, or nothing when it was already there or is not kept.
    pub fn take_in(&self, host: &str, message: &Message) -> Result<Option<Stored>> {
        self.backend().take_in(host, message)
    }

    /// Forget where a host's channel was read up to: its counter started again.
    pub fn reset_cursor(&self, host: &str) -> Result<()> {
        self.backend().reset_cursor(host)
    }

    /// Messages, newest first, and the `before` for the next page when there is one.
    pub fn inbox(&self, filter: &Filter<'_>) -> Result<(Vec<Stored>, Option<i64>)> {
        let limit = filter.limit.clamp(1, 1000);
        // One more than asked for says whether there is a next page.
        let mut messages = self.backend().inbox(filter, limit + 1)?;
        let next = if messages.len() > limit {
            messages.truncate(limit);
            messages.last().map(|message| message.seq)
        } else {
            None
        };
        Ok((messages, next))
    }

    /// Mark a message done or not. False when there is no such message.
    pub fn set_done(&self, host: &str, id: u64, done: bool) -> Result<bool> {
        self.backend().set_done(host, id, done)
    }

    // ── Key-value ───────────────────────────────────────────────────────────

    pub fn get(&self, namespace: &str, key: &str) -> Result<Option<Entry>> {
        self.backend().get(namespace, key)
    }

    /// Set a value. Returns its new revision.
    pub fn put(&self, namespace: &str, key: &str, value: &Value, when: When) -> Result<i64> {
        let text = value.to_string();
        if text.len() > VALUE_LIMIT {
            return Err(Error::TooLarge);
        }
        self.backend().put(namespace, key, &text, when, &now())
    }

    /// Remove a key. False when it was not there.
    pub fn remove(&self, namespace: &str, key: &str, when: When) -> Result<bool> {
        self.backend().remove(namespace, key, when)
    }

    /// A namespace's entries, by key.
    pub fn list(&self, namespace: &str) -> Result<Vec<Entry>> {
        self.backend().list(namespace)
    }

    // ── Jobs ────────────────────────────────────────────────────────────────

    // ── Pruning ─────────────────────────────────────────────────────────────

    /// Drop messages and ended jobs older than `days`. Key-value and state
    /// are never pruned. Returns the output files of the jobs dropped, for
    /// removing.
    pub fn prune(&self, days: u64) -> Result<Vec<String>> {
        let cutoff = rfc3339(unix_now().saturating_sub(days * 86400));
        self.backend().prune(&cutoff)
    }

    // ── State ───────────────────────────────────────────────────────────────

    /// Whether secrets and keys are sealed where they are kept.
    pub fn encrypted(&self) -> bool {
        self.backend().seals()
    }

    fn row(
        &self,
        kind: &str,
        scope: &str,
        name: &str,
        plain: &[u8],
        updated: &str,
    ) -> Result<StateRow> {
        Ok(StateRow {
            kind: kind.to_owned(),
            scope: scope.to_owned(),
            name: name.to_owned(),
            value: plain.to_vec(),
            sealed: false,
            updated: updated.to_owned(),
            label: None,
        })
    }

    /// The value of a row: the backend gives it plain.
    fn opened(&self, row: StateRow) -> Result<Vec<u8>> {
        Ok(row.value)
    }

    pub fn read(&self, kind: &str, scope: &str, name: &str) -> Result<Option<Vec<u8>>> {
        match self.backend().state(kind, scope, name)? {
            Some(row) => self.opened(row).map(Some),
            None => Ok(None),
        }
    }

    pub fn read_json(&self, kind: &str, scope: &str, name: &str) -> Result<Option<Value>> {
        match self.read(kind, scope, name)? {
            Some(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
                Error::Database(format!("{kind} {name:?} in the store is not JSON: {error}"))
            }),
            None => Ok(None),
        }
    }

    /// Insert or replace a value; it is updated now.
    pub fn write(&self, kind: &str, scope: &str, name: &str, value: &[u8]) -> Result<()> {
        let row = self.row(kind, scope, name, value, &now())?;
        self.backend().put_state(&row)
    }

    pub fn write_json(&self, kind: &str, scope: &str, name: &str, value: &Value) -> Result<()> {
        self.write(kind, scope, name, value.to_string().as_bytes())
    }

    /// Remove a value. False when it was not there.
    pub fn forget(&self, kind: &str, scope: &str, name: &str) -> Result<bool> {
        self.backend().remove_state(kind, scope, name)
    }

    /// The names a scope has values of a kind for, in order. Nothing is unsealed.
    pub fn names(&self, kind: &str, scope: &str) -> Result<Vec<String>> {
        let rows = self.backend().states(kind, scope)?;
        Ok(rows.into_iter().map(|row| row.name).collect())
    }

    /// A scope's values of a kind, unsealed, by name.
    pub fn entries(&self, kind: &str, scope: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let rows = self.backend().states(kind, scope)?;
        rows.into_iter()
            .map(|row| {
                let name = row.name.clone();
                Ok((name, self.opened(row)?))
            })
            .collect()
    }

    /// In one transaction: everything of `scope` in `kinds` is replaced by
    /// `items` (kind, name, value).
    pub fn replace(
        &self,
        scope: &str,
        kinds: &[&str],
        items: &[(&str, &str, Vec<u8>)],
    ) -> Result<()> {
        let updated = now();
        let rows = items
            .iter()
            .map(|(kind, name, value)| self.row(kind, scope, name, value, &updated))
            .collect::<Result<Vec<_>>>()?;
        self.backend().replace_states(scope, kinds, &rows)
    }

    /// Several writes (kind, scope, name, value) and removals (kind, scope,
    /// name) in one transaction, for a change that must not be half done.
    pub fn change(
        &self,
        writes: &[(&str, &str, &str, Vec<u8>)],
        removals: &[(&str, &str, &str)],
    ) -> Result<()> {
        let mut batch = Batch::default();
        for (kind, scope, name, value) in writes {
            batch.write(kind, scope, name, value.clone());
        }
        for (kind, scope, name) in removals {
            batch.remove(kind, scope, name);
        }
        self.apply(&batch)
    }

    /// Make the changes of a batch in one transaction.
    pub fn apply(&self, batch: &Batch) -> Result<()> {
        let updated = now();
        let mut change = Change {
            removals: batch.removals.clone(),
            owners: batch.owners.clone(),
            hosts: batch.hosts.clone(),
            jobs: batch.jobs.clone(),
            ..Change::default()
        };
        for (kind, scope, name, label, value) in &batch.writes {
            if let Some(label) = label {
                check_label(kind, label)?;
            }
            let mut row = self.row(kind, scope, name, value, &updated)?;
            row.label = label.clone();
            change.writes.push(row);
        }
        let result = self.backend().change_states(&change);
        result.map_err(|error| taken(error, &batch.writes))
    }

    // ── Things with ids ─────────────────────────────────────────────────────

    /// The id of the thing of a kind with this name.
    pub fn id_of(&self, kind: &str, scope: &str, label: &str) -> Result<Option<String>> {
        Ok(self
            .backend()
            .labelled(kind, scope, label)?
            .map(|row| row.name))
    }

    /// The name of the thing of a kind with this id. Nothing is unsealed.
    pub fn label_of(&self, kind: &str, scope: &str, id: &str) -> Result<Option<String>> {
        Ok(self
            .backend()
            .state(kind, scope, id)?
            .and_then(|row| row.label))
    }

    /// The thing of a kind that a name or id gives: an id with the prefix
    /// of its kind is that id alone; a bare id is that id when a thing has
    /// it; anything else is a name. A prefix of another kind is refused.
    pub fn find(&self, kind: &str, scope: &str, given: &str) -> Result<Option<Found>> {
        let found = |row: StateRow| {
            row.label.clone().map(|name| Found {
                id: row.name.clone(),
                name,
            })
        };
        let Some(id_kind) = id_kind(kind) else {
            return Ok(self.backend().labelled(kind, scope, given)?.and_then(found));
        };
        match ids::read(id_kind, given).map_err(Error::Given)? {
            ids::Given::Prefixed(id) => Ok(self.backend().state(kind, scope, &id)?.and_then(found)),
            ids::Given::Bare(id) => {
                // The lock is let go before the name is looked up.
                let row = self.backend().state(kind, scope, &id)?;
                match row {
                    Some(row) if row.label.is_some() => Ok(found(row)),
                    _ => Ok(self.backend().labelled(kind, scope, given)?.and_then(found)),
                }
            }
            ids::Given::Name(name) => {
                Ok(self.backend().labelled(kind, scope, &name)?.and_then(found))
            }
        }
    }

    /// The things of a kind in a scope, by name.
    pub fn labels(&self, kind: &str, scope: &str) -> Result<Vec<Found>> {
        let mut found: Vec<Found> = self
            .backend()
            .states(kind, scope)?
            .into_iter()
            .filter_map(|row| {
                Some(Found {
                    name: row.label?,
                    id: row.name,
                })
            })
            .collect();
        found.sort_by(|one, other| one.name.cmp(&other.name));
        Ok(found)
    }

    /// The things of a kind in a scope with their values, unsealed, by name.
    pub fn labelled_entries(&self, kind: &str, scope: &str) -> Result<Vec<(Found, Vec<u8>)>> {
        let mut found = vec![];
        for row in self.backend().states(kind, scope)? {
            let Some(name) = row.label.clone() else {
                continue;
            };
            let id = row.name.clone();
            found.push((Found { id, name }, self.opened(row)?));
        }
        found.sort_by(|(one, _), (other, _)| one.name.cmp(&other.name));
        Ok(found)
    }

    /// Insert or replace the value of a thing with an id, under its name.
    pub fn write_labelled(
        &self,
        kind: &str,
        scope: &str,
        id: &str,
        label: &str,
        value: &[u8],
    ) -> Result<()> {
        let mut batch = Batch::default();
        batch.write_labelled(kind, scope, id, label, value.to_vec());
        self.apply(&batch)
    }

    /// Give a thing another name, which no other thing of its kind has.
    /// False when there is no thing with the id.
    pub fn relabel(&self, kind: &str, scope: &str, id: &str, label: &str) -> Result<bool> {
        check_label(kind, label)?;
        self.backend()
            .relabel(kind, scope, id, label, &now())
            .map_err(|error| match is_taken(&error) {
                true => Error::Taken {
                    kind: kind.to_owned(),
                    label: label.to_owned(),
                },
                false => error,
            })
    }
}

/// A write of a batch: (kind, scope, name, label, plain value).
type Write = (String, String, String, Option<String>, Vec<u8>);

/// Several changes to `state` to make in one transaction, with
/// [`Store::apply`].
#[derive(Debug, Clone, Default)]
pub struct Batch {
    writes: Vec<Write>,
    removals: Vec<(String, String, String)>,
    owners: Vec<(String, String)>,
    hosts: Vec<(String, String)>,
    jobs: Vec<(String, String, String)>,
}

impl Batch {
    /// Insert or replace a value; a row with a label keeps it.
    pub fn write(&mut self, kind: &str, scope: &str, name: &str, value: Vec<u8>) -> &mut Self {
        let key = (kind.to_owned(), scope.to_owned(), name.to_owned());
        self.writes.push((key.0, key.1, key.2, None, value));
        self
    }

    /// Insert or replace the value of a thing with an id, under its name.
    pub fn write_labelled(
        &mut self,
        kind: &str,
        scope: &str,
        id: &str,
        label: &str,
        value: Vec<u8>,
    ) -> &mut Self {
        let key = (kind.to_owned(), scope.to_owned(), id.to_owned());
        self.writes
            .push((key.0, key.1, key.2, Some(label.to_owned()), value));
        self
    }

    pub fn remove(&mut self, kind: &str, scope: &str, name: &str) -> &mut Self {
        self.removals
            .push((kind.to_owned(), scope.to_owned(), name.to_owned()));
        self
    }

    /// Remove every row that belongs to the thing with the id `owner`.
    pub fn forget_owned(&mut self, scope: &str, owner: &str) -> &mut Self {
        self.owners.push((scope.to_owned(), owner.to_owned()));
        self
    }

    /// Give the messages and the channel cursor of the host `old` to `new`.
    pub fn move_host(&mut self, old: &str, new: &str) -> &mut Self {
        self.hosts.push((old.to_owned(), new.to_owned()));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.writes.is_empty()
            && self.removals.is_empty()
            && self.owners.is_empty()
            && self.hosts.is_empty()
            && self.jobs.is_empty()
    }
}

/// Whether an error of a backend is a label taken by another row.
fn is_taken(error: &Error) -> bool {
    matches!(error, Error::Database(message) if message.contains("state_label") || message.contains("state.label"))
}

/// The error of a change, with a label that was taken said as such.
fn taken(error: Error, writes: &[Write]) -> Error {
    if !is_taken(&error) {
        return error;
    }
    match writes.iter().find(|write| write.3.is_some()) {
        Some((kind, _, _, Some(label), _)) => Error::Taken {
            kind: kind.clone(),
            label: label.clone(),
        },
        _ => error,
    }
}
