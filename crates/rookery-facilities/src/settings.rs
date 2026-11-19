//! `settings.*`: a plugin's settings, resolved for the caller as their own value, else the global
//! one, else the plugin's default.

use rookery_kernel::{Call, Caller, Empty, Level, Methods, RpcError};
use rookery_proto::facilities::{
    Setting, SettingList, SettingRef, SettingReset, SettingScope, SettingSet, SettingValue,
    SettingsOf,
};
use rookery_store::settings::{Resolved, Scope};

use crate::own_user;

fn setting(r: Resolved) -> Setting {
    let scope = match r.scope.as_str() {
        "user" => SettingScope::User,
        "global" => SettingScope::Global,
        _ => SettingScope::Default,
    };
    Setting {
        key: r.key,
        value: r.value,
        scope,
    }
}

fn check_key(key: &str) -> Result<(), RpcError> {
    if key.is_empty() || key.len() > 200 {
        return Err(RpcError::invalid("a setting's key is 1 to 200 bytes"));
    }
    Ok(())
}

/// Where a write goes, and whether the caller may write there: a default is the plugin's or an admin's,
/// a global value an admin's, a user's value their own.
fn scope(call: &Call, plugin: &str, scope: SettingScope) -> Result<Scope, RpcError> {
    match scope {
        SettingScope::Default => {
            call.plugin(Some(plugin))?;
            Ok(Scope::Default)
        }
        SettingScope::Global => {
            call.require_admin()?;
            Ok(Scope::Global)
        }
        SettingScope::User => Ok(Scope::User(own_user(call)?)),
    }
}

fn user_of(call: &Call) -> Option<uuid::Uuid> {
    match &call.caller {
        Caller::User { user, .. } => uuid::Uuid::parse_str(&user.id).ok(),
        _ => None,
    }
}

pub fn register(methods: &mut Methods) {
    methods
        .add("settings.get", |call: Call, p: SettingRef| async move {
            rookery_kernel::methods::named_plugin(&call, &p.plugin)?;
            let found = call.store().settings().get(&p.plugin, &p.key, user_of(&call)).await?.map(setting);
            Ok(match found {
                Some(s) => SettingValue { key: s.key, value: s.value, scope: Some(s.scope) },
                None => SettingValue { key: p.key, value: serde_json::Value::Null, scope: None },
            })
        })
        .summary("A plugin's setting as it resolves for the caller: their own, else the global one, else the default.")
        .level(Level::Own);
    methods
        .add("settings.list", |call: Call, p: SettingsOf| async move {
            rookery_kernel::methods::named_plugin(&call, &p.plugin)?;
            let all = call
                .store()
                .settings()
                .list(&p.plugin, user_of(&call))
                .await?;
            Ok(SettingList {
                entries: all.into_iter().map(setting).collect(),
            })
        })
        .summary("Every setting of a plugin as it resolves for the caller.")
        .level(Level::Own);
    methods
        .add("settings.set", |call: Call, p: SettingSet| async move {
            rookery_kernel::methods::named_plugin(&call, &p.plugin)?;
            check_key(&p.key)?;
            let at = scope(&call, &p.plugin, p.scope)?;
            call.store().settings().set(&p.plugin, &p.key, at, &p.value, &call.audit_name()).await?;
            if at == Scope::Global {
                call.audit("settings.set", Some(&format!("plugin://{}", p.plugin)), serde_json::json!({ "key": p.key })).await;
            }
            Ok(Empty {})
        })
        .summary("Sets a plugin's setting: its default (the plugin, or an admin), the global value (an admin) or the caller's own.")
        .level(Level::Own);
    methods
        .add("settings.reset", |call: Call, p: SettingReset| async move {
            rookery_kernel::methods::named_plugin(&call, &p.plugin)?;
            let at = scope(&call, &p.plugin, p.scope)?;
            call.store().settings().reset(&p.plugin, &p.key, at).await?;
            Ok(Empty {})
        })
        .summary("Removes a setting's value at one scope, so the next one shows through.")
        .level(Level::Own);
}
