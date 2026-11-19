//! `storage.*`: a plugin's key-value storage, global or a person's, with a revision on every value and
//! conditional writes.

use rookery_kernel::{Call, Level, Methods, RpcError};
use rookery_proto::facilities::{
    Deleted, Revision, Scope, StorageEntries, StorageEntry, StorageList, StorageRef, StorageSet,
    StorageValue,
};
use uuid::Uuid;

use crate::{own_user, time};

fn entry(e: rookery_store::kv::Entry) -> StorageEntry {
    StorageEntry {
        key: e.key,
        value: e.value,
        revision: e.revision,
        updated_at: time(e.updated_at),
    }
}

fn scope(call: &Call, scope: Scope) -> Result<Option<Uuid>, RpcError> {
    match scope {
        Scope::Global => Ok(None),
        Scope::User => Ok(Some(own_user(call)?)),
    }
}

pub fn register(methods: &mut Methods) {
    methods
        .add("storage.get", |call: Call, p: StorageRef| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let found = call
                .store()
                .kv()
                .get(&plugin, scope(&call, p.scope)?, &p.key)
                .await?;
            Ok(match found {
                Some(e) => StorageValue {
                    key: e.key,
                    value: e.value,
                    revision: e.revision,
                    updated_at: Some(time(e.updated_at)),
                },
                None => StorageValue {
                    key: p.key,
                    value: serde_json::Value::Null,
                    revision: 0,
                    updated_at: None,
                },
            })
        })
        .summary("A stored value and its revision.")
        .level(Level::Plugin);
    methods
        .add("storage.set", |call: Call, p: StorageSet| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let size = serde_json::to_vec(&p.value).map(|v| v.len()).unwrap_or(0);
            if size > 1024 * 1024 {
                return Err(RpcError::invalid("a stored value is at most 1 MiB of JSON"));
            }
            let revision = call.store().kv().set(&plugin, scope(&call, p.scope)?, &p.key, &p.value, p.revision).await?;
            Ok(Revision { revision })
        })
        .summary("Stores a value. With revision, only if the key is at it, 0 for a key not there yet; CONFLICT otherwise.")
        .level(Level::Plugin);
    methods
        .add("storage.delete", |call: Call, p: StorageRef| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let deleted = call
                .store()
                .kv()
                .delete(&plugin, scope(&call, p.scope)?, &p.key, p.revision)
                .await?;
            Ok(Deleted { deleted })
        })
        .summary("Removes a stored value; with revision, only at it.")
        .level(Level::Plugin);
    methods
        .add("storage.list", |call: Call, p: StorageList| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let entries = call
                .store()
                .kv()
                .list(
                    &plugin,
                    scope(&call, p.scope)?,
                    &p.prefix,
                    p.after.as_deref(),
                    p.limit.unwrap_or(100),
                )
                .await?;
            Ok(StorageEntries {
                entries: entries.into_iter().map(entry).collect(),
            })
        })
        .summary("The stored values whose keys start with a prefix, in order.")
        .level(Level::Plugin);
}
