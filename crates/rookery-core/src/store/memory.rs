//! A store in memory, for tests: the same rules as the server's tables, kept in maps.

use super::{Backend, Change, Entry, Error, Filter, Result, StateRow, Stored, When, owned_by};
use rookery_api::{Keep, Message};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct MemoryBackend {
    /// By (kind, scope, name).
    state: BTreeMap<(String, String, String), StateRow>,
    /// By (host, id): the message, its seq and whether it is done.
    messages: BTreeMap<(String, u64), (i64, Message, bool)>,
    seq: i64,
    cursors: BTreeMap<String, u64>,
    /// By (namespace, key).
    kv: BTreeMap<(String, String), Entry>,
}

fn key(row: &StateRow) -> (String, String, String) {
    (row.kind.clone(), row.scope.clone(), row.name.clone())
}

impl MemoryBackend {
    fn label_taken(&self, row: &StateRow) -> bool {
        let Some(label) = &row.label else {
            return false;
        };
        self.state.values().any(|other| {
            other.kind == row.kind
                && other.scope == row.scope
                && other.name != row.name
                && other.label.as_deref() == Some(label)
        })
    }

    fn put(&mut self, row: &StateRow) -> Result<()> {
        if self.label_taken(row) {
            return Err(Error::Taken {
                kind: row.kind.clone(),
                label: row.label.clone().unwrap_or_default(),
            });
        }
        let mut row = row.clone();
        // A write without a label keeps the one the row has.
        if row.label.is_none()
            && let Some(before) = self.state.get(&key(&row))
        {
            row.label = before.label.clone();
        }
        self.state.insert(key(&row), row);
        Ok(())
    }
}

impl Backend for MemoryBackend {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn cursor(&mut self, host: &str) -> Result<u64> {
        Ok(self.cursors.get(host).copied().unwrap_or(0))
    }

    fn take_in(&mut self, host: &str, message: &Message) -> Result<Option<Stored>> {
        let mut taken = None;
        if message.keep != Keep::None {
            if message.keep == Keep::Last {
                self.messages.retain(|(h, id), (_, kept, _)| {
                    !(h == host
                        && kept.topic == message.topic
                        && kept.keep == Keep::Last
                        && *id < message.id)
                });
            }
            let at = (host.to_owned(), message.id);
            if !self.messages.contains_key(&at) {
                self.seq += 1;
                self.messages.insert(at, (self.seq, message.clone(), false));
                taken = Some(Stored {
                    seq: self.seq,
                    host: host.to_owned(),
                    message: message.clone(),
                    done: false,
                });
            }
        }
        let cursor = self.cursors.entry(host.to_owned()).or_default();
        *cursor = (*cursor).max(message.id);
        Ok(taken)
    }

    fn reset_cursor(&mut self, host: &str) -> Result<()> {
        self.cursors.remove(host);
        Ok(())
    }

    fn inbox(&mut self, filter: &Filter<'_>, limit: usize) -> Result<Vec<Stored>> {
        let topic = filter.topic.unwrap_or("");
        let mut found: Vec<Stored> = self
            .messages
            .iter()
            .filter(|((host, _), (seq, message, done))| {
                filter.host.is_none_or(|wanted| wanted == host)
                    && message.topic.starts_with(topic)
                    && filter.before.is_none_or(|before| *seq < before)
                    && filter.done.is_none_or(|wanted| wanted == *done)
            })
            .map(|((host, _), (seq, message, done))| Stored {
                seq: *seq,
                host: host.clone(),
                message: message.clone(),
                done: *done,
            })
            .collect();
        found.sort_by_key(|stored| std::cmp::Reverse(stored.seq));
        found.truncate(limit);
        Ok(found)
    }

    fn set_done(&mut self, host: &str, id: u64, done: bool) -> Result<bool> {
        match self.messages.get_mut(&(host.to_owned(), id)) {
            Some(kept) => {
                kept.2 = done;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn get(&mut self, namespace: &str, key: &str) -> Result<Option<Entry>> {
        Ok(self
            .kv
            .get(&(namespace.to_owned(), key.to_owned()))
            .cloned())
    }

    fn put(
        &mut self,
        namespace: &str,
        key: &str,
        text: &str,
        when: When,
        updated: &str,
    ) -> Result<i64> {
        let at = (namespace.to_owned(), key.to_owned());
        let current = self.kv.get(&at).map(|entry| entry.revision);
        let allowed = match when {
            When::Always => true,
            When::At(revision) => current == Some(revision),
            When::New => current.is_none(),
        };
        if !allowed {
            return Err(Error::Conflict(current));
        }
        let revision = current.unwrap_or(0) + 1;
        self.kv.insert(
            at,
            Entry {
                key: key.to_owned(),
                value: serde_json::from_str(text).unwrap_or(serde_json::Value::Null),
                revision,
                updated: updated.to_owned(),
            },
        );
        Ok(revision)
    }

    fn remove(&mut self, namespace: &str, key: &str, when: When) -> Result<bool> {
        let at = (namespace.to_owned(), key.to_owned());
        let current = self.kv.get(&at).map(|entry| entry.revision);
        if let When::At(revision) = when
            && current != Some(revision)
        {
            return Err(Error::Conflict(current));
        }
        Ok(self.kv.remove(&at).is_some())
    }

    fn list(&mut self, namespace: &str) -> Result<Vec<Entry>> {
        Ok(self
            .kv
            .iter()
            .filter(|((ns, _), _)| ns == namespace)
            .map(|(_, entry)| entry.clone())
            .collect())
    }

    fn prune(&mut self, cutoff: &str) -> Result<Vec<String>> {
        self.messages
            .retain(|_, (_, message, _)| message.time.as_str() >= cutoff);
        Ok(vec![])
    }

    fn state(&mut self, kind: &str, scope: &str, name: &str) -> Result<Option<StateRow>> {
        Ok(self
            .state
            .get(&(kind.to_owned(), scope.to_owned(), name.to_owned()))
            .cloned())
    }

    fn states(&mut self, kind: &str, scope: &str) -> Result<Vec<StateRow>> {
        Ok(self
            .state
            .values()
            .filter(|row| row.kind == kind && row.scope == scope)
            .cloned()
            .collect())
    }

    fn put_state(&mut self, row: &StateRow) -> Result<()> {
        self.put(row)
    }

    fn remove_state(&mut self, kind: &str, scope: &str, name: &str) -> Result<bool> {
        Ok(self
            .state
            .remove(&(kind.to_owned(), scope.to_owned(), name.to_owned()))
            .is_some())
    }

    fn replace_states(&mut self, scope: &str, kinds: &[&str], rows: &[StateRow]) -> Result<()> {
        let before = self.state.clone();
        self.state
            .retain(|_, row| !(row.scope == scope && kinds.contains(&row.kind.as_str())));
        for row in rows {
            if let Err(error) = self.put(row) {
                self.state = before;
                return Err(error);
            }
        }
        Ok(())
    }

    fn all_states(&mut self) -> Result<Vec<StateRow>> {
        Ok(self.state.values().cloned().collect())
    }

    fn labelled(&mut self, kind: &str, scope: &str, label: &str) -> Result<Option<StateRow>> {
        Ok(self
            .state
            .values()
            .find(|row| {
                row.kind == kind && row.scope == scope && row.label.as_deref() == Some(label)
            })
            .cloned())
    }

    fn relabel(
        &mut self,
        kind: &str,
        scope: &str,
        name: &str,
        label: &str,
        updated: &str,
    ) -> Result<bool> {
        let at = (kind.to_owned(), scope.to_owned(), name.to_owned());
        let Some(mut row) = self.state.get(&at).cloned() else {
            return Ok(false);
        };
        row.label = Some(label.to_owned());
        row.updated = updated.to_owned();
        self.put(&row)?;
        Ok(true)
    }

    fn change_states(&mut self, change: &Change) -> Result<()> {
        // All of it or nothing.
        let (state, messages, cursors) = (
            self.state.clone(),
            self.messages.clone(),
            self.cursors.clone(),
        );
        let done = (|| {
            for (kind, scope, name) in &change.removals {
                self.state
                    .remove(&(kind.clone(), scope.clone(), name.clone()));
            }
            for (scope, owner) in &change.owners {
                self.state.retain(|_, row| {
                    !(row.scope == *scope && row.label.is_none() && owned_by(&row.name, owner))
                });
            }
            for row in &change.writes {
                self.put(row)?;
            }
            for (old, new) in &change.hosts {
                let moved: Vec<_> = self
                    .messages
                    .iter()
                    .filter(|((host, _), _)| host == old)
                    .map(|((_, id), kept)| ((new.clone(), *id), kept.clone()))
                    .collect();
                self.messages.retain(|(host, _), _| host != old);
                self.messages.extend(moved);
                if let Some(cursor) = self.cursors.remove(old) {
                    self.cursors.insert(new.clone(), cursor);
                }
            }
            Ok(())
        })();
        if done.is_err() {
            self.state = state;
            self.messages = messages;
            self.cursors = cursors;
        }
        done
    }
}
