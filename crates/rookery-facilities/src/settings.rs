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
        "space" => SettingScope::Space,
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
/// a global value an admin's, a space's value a manager's of it, a user's value their own.
async fn scope(
    call: &Call,
    plugin: &str,
    scope: SettingScope,
    space: Option<&str>,
) -> Result<Scope, RpcError> {
    match scope {
        SettingScope::Space => Ok(Scope::Space(
            crate::space_held(call, space, rookery_kernel::Grant::Manage).await?,
        )),
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

/// The space a read resolves in, which the caller holds view on.
async fn resolve_in(call: &Call, space: Option<&str>) -> Result<Option<uuid::Uuid>, RpcError> {
    match space {
        None => Ok(None),
        Some(_) => Ok(Some(
            crate::space_held(call, space, rookery_kernel::Grant::View).await?,
        )),
    }
}

fn user_of(call: &Call) -> Option<uuid::Uuid> {
    match &call.caller {
        Caller::User { user, .. } => uuid::Uuid::parse_str(&user.id).ok(),
        _ => None,
    }
}

async fn audit_change(call: &Call, action: &str, plugin: &str, key: &str, at: Scope) {
    match at {
        Scope::Global => {
            call.audit(
                action,
                Some(&format!("plugin://{plugin}")),
                serde_json::json!({ "key": key }),
            )
            .await;
        }
        Scope::Space(space) => {
            call.audit(
                action,
                Some(&format!("space://{space}")),
                serde_json::json!({ "plugin": plugin, "key": key }),
            )
            .await;
        }
        _ => {}
    }
}

pub fn register(methods: &mut Methods) {
    methods
        .add("settings.get", |call: Call, p: SettingRef| async move {
            rookery_kernel::methods::named_plugin(&call, &p.plugin)?;
            let space = resolve_in(&call, p.space.as_deref()).await?;
            let found = call.store().settings().get_in(&p.plugin, &p.key, user_of(&call), space).await?.map(setting);
            Ok(match found {
                Some(s) => SettingValue { key: s.key, value: s.value, scope: Some(s.scope) },
                None => SettingValue { key: p.key, value: serde_json::Value::Null, scope: None },
            })
        })
        .summary("A plugin's setting as it resolves for the caller: their own, else the space's they name, else the global one, else the default.")
        .level(Level::Own);
    methods
        .add("settings.list", |call: Call, p: SettingsOf| async move {
            rookery_kernel::methods::named_plugin(&call, &p.plugin)?;
            let space = resolve_in(&call, p.space.as_deref()).await?;
            let all = call
                .store()
                .settings()
                .list_in(&p.plugin, user_of(&call), space)
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
            let at = scope(&call, &p.plugin, p.scope, p.space.as_deref()).await?;
            call.store().settings().set(&p.plugin, &p.key, at, &p.value, &call.audit_name()).await?;
            // What others read is audited: the global value, and a space's on the space.
            audit_change(&call, "settings.set", &p.plugin, &p.key, at).await;
            Ok(Empty {})
        })
        .summary("Sets a plugin's setting: its default (the plugin, or an admin), the global value (an admin), a space's (its managers) or the caller's own.")
        .level(Level::Own);
    methods
        .add("settings.reset", |call: Call, p: SettingReset| async move {
            rookery_kernel::methods::named_plugin(&call, &p.plugin)?;
            let at = scope(&call, &p.plugin, p.scope, p.space.as_deref()).await?;
            call.store().settings().reset(&p.plugin, &p.key, at).await?;
            audit_change(&call, "settings.reset", &p.plugin, &p.key, at).await;
            Ok(Empty {})
        })
        .summary("Removes a setting's value at one scope, so the next one shows through.")
        .level(Level::Own);
}
