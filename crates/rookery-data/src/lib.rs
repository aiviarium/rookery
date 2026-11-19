//! The data layer's methods: `data.*` over collections, live queries and the change feed, references,
//! and the runtime of the collections and migrations plugins declare ([`plugins`]).
//!
//! Reads and writes go to the store with the caller, which applies the collection's scope and access
//! levels: the levels a plugin's manifest declares per collection. So every `data.*` method is open to
//! anyone signed in, and the store refuses what the caller may not do.
//!
//! Plugins name their own collections only; people and admins name the plugin. Rookery's own data is
//! the plugin `rookery`, which no plugin may write through `data.*`.

pub mod data;
pub mod entities;
pub mod home;
pub mod live;
pub mod plugins;
pub mod spaces;

use rookery_kernel::{Call, Caller, Methods, RpcError};
use rookery_proto::data::DataTarget;
use rookery_store::Caller as StoreCaller;
use rookery_store::collections::system::PLUGIN as ROOKERY;
use uuid::Uuid;

pub fn register(methods: &mut Methods) {
    data::register(methods);
    entities::register(methods);
    home::register(methods);
    live::register(methods);
    plugins::register(methods);
    spaces::register(methods);
}

pub(crate) fn parse_id(what: &str, id: &str) -> Result<Uuid, RpcError> {
    Uuid::parse_str(id).map_err(|_| RpcError::invalid(format!("{what}: {id:?} is not an id")))
}

/// The store's caller for a call, in `space` when it names one, else in the space its connection entered.
pub fn store_caller(call: &Call, space: Option<&str>) -> Result<StoreCaller, RpcError> {
    let caller = match &call.caller {
        Caller::User { user, .. } => {
            let id = parse_id("user", &user.id)?;
            let mut c = StoreCaller::user(id);
            c.admin = user.admin;
            c
        }
        Caller::Local | Caller::System(_) => StoreCaller {
            principal: call.audit_name(),
            ..StoreCaller::system()
        },
        Caller::Plugin(plugin) => StoreCaller::plugin(plugin),
    };
    Ok(match space {
        Some(s) => caller.in_space(parse_id("space", s)?),
        None => caller.with_space(spaces::current(call)),
    })
}

/// The plugin whose collection a call is about, and the store's caller for it. A plugin acts on its own
/// collections only, as their admin; the plugin `rookery` is Rookery's own data.
pub async fn target(call: &Call, t: &DataTarget) -> Result<(String, StoreCaller), RpcError> {
    let plugin = match (&call.caller, t.plugin.as_deref()) {
        (Caller::Plugin(own), None) => own.clone(),
        (Caller::Plugin(own), Some(named)) if named == own => own.clone(),
        (Caller::Plugin(own), Some(named)) => {
            return Err(RpcError::forbidden(format!(
                "the plugin {own} uses its own collections, not those of {named}"
            )));
        }
        (_, Some(ROOKERY)) => ROOKERY.to_owned(),
        (_, Some(named)) => rookery_kernel::methods::plugin_id(named)?,
        (_, None) => {
            return Err(RpcError::invalid(
                "name the plugin whose collection this is in plugin",
            ));
        }
    };
    let mut caller = store_caller(call, t.space.as_deref())?;
    if matches!(&call.caller, Caller::Plugin(own) if *own == plugin && own != ROOKERY) {
        // Admin of its own collections only: what it reads of another plugin's, as through `include`,
        // it reads as a plugin.
        caller = caller.admin_of(plugin.clone());
    }
    Ok((plugin, caller))
}

/// [`target`] for a write: Rookery's own collections are written by Rookery's own methods (storage.*,
/// layouts, its typed tables), never through `data.*`.
pub async fn write_target(call: &Call, t: &DataTarget) -> Result<(String, StoreCaller), RpcError> {
    let (plugin, caller) = target(call, t).await?;
    if plugin == ROOKERY {
        return Err(RpcError::forbidden(
            "Rookery's own collections are written through their own methods, not data.*",
        ));
    }
    Ok((plugin, caller))
}

pub(crate) fn time(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub(crate) fn rpc(e: rookery_store::Error) -> RpcError {
    e.into()
}
