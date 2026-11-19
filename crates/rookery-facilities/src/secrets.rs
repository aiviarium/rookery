//! `secrets.*`: a plugin's secrets, sealed in their rows. A global secret is the plugin's or an admin's,
//! a user's secret their own. A server with no key refuses them with `UNAVAILABLE`.

use rookery_kernel::{Call, Caller, Empty, Level, Methods, RpcError};
use rookery_proto::facilities::{
    Scope as WireScope, SecretInfo, SecretList, SecretRef, SecretRename, SecretSet, SecretValue,
    SecretsOf,
};
use rookery_store::secrets::Scope;

use crate::{own_user, time};

fn scope(call: &Call, plugin: &str, scope: WireScope) -> Result<Scope, RpcError> {
    rookery_kernel::methods::plugin_id(plugin)?;
    match scope {
        WireScope::Global => {
            call.plugin(Some(plugin))?;
            Ok(Scope::Global)
        }
        WireScope::User => Ok(Scope::User(own_user(call)?)),
    }
}

fn resource(plugin: &str, name: &str) -> String {
    format!("secret://{plugin}/{name}")
}

pub fn register(methods: &mut Methods) {
    methods
        .add("secrets.get", |call: Call, p: SecretRef| async move {
            let at = scope(&call, &p.plugin, p.scope)?;
            let value = call.store().secrets().get(&p.plugin, &p.name, at).await?;
            let value = value
                .map(|v| {
                    String::from_utf8(v).map_err(|_| RpcError::internal("a secret is not text"))
                })
                .transpose()?;
            Ok(SecretValue { value })
        })
        .summary(
            "A secret's value: a global one for its plugin or an admin, a user's for that person.",
        )
        .level(Level::Own);
    methods
        .add("secrets.set", |call: Call, p: SecretSet| async move {
            let at = scope(&call, &p.plugin, p.scope)?;
            if p.value.len() > 64 * 1024 {
                return Err(RpcError::invalid("a secret is at most 64 KiB"));
            }
            call.store()
                .secrets()
                .set(&p.plugin, &p.name, at, p.value.as_bytes())
                .await?;
            call.audit(
                "secret.set",
                Some(&resource(&p.plugin, &p.name)),
                serde_json::json!({ "scope": p.scope }),
            )
            .await;
            Ok(Empty {})
        })
        .summary("Sets a secret, new or changed. Its value is sealed and never shown in a list.")
        .level(Level::Own);
    methods
        .add("secrets.list", |call: Call, p: SecretsOf| async move {
            let user = call.user().and_then(|u| uuid::Uuid::parse_str(&u.id).ok());
            let plugin = match &call.caller {
                Caller::Plugin(own) => Some(own.clone()),
                _ => p.plugin.clone(),
            };
            let all = call.store().secrets().list(plugin.as_deref(), user).await?;
            // A person who is not an admin sees only their own.
            let admin = call.is_admin() || matches!(call.caller, Caller::Plugin(_));
            let secrets = all
                .into_iter()
                .filter(|s| admin || s.scope == "user")
                .map(|s| SecretInfo {
                    scope: if s.scope == "user" {
                        WireScope::User
                    } else {
                        WireScope::Global
                    },
                    plugin: s.plugin,
                    name: s.name,
                    key_id: s.key_id,
                    updated_at: time(s.updated_at),
                })
                .collect();
            Ok(SecretList { secrets })
        })
        .summary("The secrets the caller may see, without their values.")
        .level(Level::Own);
    methods
        .add("secrets.delete", |call: Call, p: SecretRef| async move {
            let at = scope(&call, &p.plugin, p.scope)?;
            if !call
                .store()
                .secrets()
                .delete(&p.plugin, &p.name, at)
                .await?
            {
                return Err(RpcError::not_found(format!("no secret {}", p.name)));
            }
            call.audit(
                "secret.delete",
                Some(&resource(&p.plugin, &p.name)),
                serde_json::json!({ "scope": p.scope }),
            )
            .await;
            Ok(Empty {})
        })
        .summary("Removes a secret.")
        .level(Level::Own);
    methods
        .add("secrets.rename", |call: Call, p: SecretRename| async move {
            let at = scope(&call, &p.plugin, p.scope)?;
            call.store()
                .secrets()
                .rename(&p.plugin, &p.name, at, &p.new_name)
                .await?;
            call.audit(
                "secret.rename",
                Some(&resource(&p.plugin, &p.new_name)),
                serde_json::json!({ "from": p.name }),
            )
            .await;
            Ok(Empty {})
        })
        .summary("Gives a secret another name.")
        .level(Level::Own);
}
