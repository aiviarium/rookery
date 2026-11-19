//! `plugins.*`: what is installed, installing, updating and removing through Hatchery, asking for
//! updates, starting a host half and calling the methods it registered.

use std::sync::Arc;

use rookery_kernel::{Call, Empty, Kernel, Level, Methods, RpcError};
use serde_json::json;

use crate::Plugins;
use crate::wire::{
    PluginAdd, PluginInfo, PluginList, PluginNewer, PluginRef, PluginReport, PluginRpc,
    PluginState, PluginUpdate, PluginUpdates,
};

fn plugins(kernel: &Kernel) -> Result<Arc<Plugins>, RpcError> {
    kernel
        .get_facility::<Plugins>()
        .ok_or_else(|| RpcError::unavailable("this server has no plugin folder"))
}

fn report(plugins: &Plugins, lines: Vec<String>) -> Result<PluginReport, RpcError> {
    Ok(PluginReport {
        lines,
        plugins: plugins.infos(),
    })
}

/// Adds `plugins.list`, `plugins.add`, `plugins.update`, `plugins.remove`, `plugins.check`,
/// `plugins.activate` and `plugins.rpc`. When the server leads it reads the plugin folder, records it in
/// `rookery.plugins` and starts the host halves that start with it, if a [`crate::Setup`] was set.
pub fn register(methods: &mut Methods) {
    methods.on_started(|kernel| {
        let Some(setup) = kernel.get_facility::<crate::Setup>() else {
            return;
        };
        if kernel.get_facility::<Plugins>().is_none() {
            kernel.set_facility(Plugins::new((*setup).clone()));
        }
        let kernel = kernel.clone();
        tokio::spawn(async move {
            if let Ok(plugins) = plugins(&kernel)
                && let Err(e) = plugins.start(&kernel).await
            {
                tracing::error!(error = %e, "reading the plugins failed");
            }
        });
    });
    methods
        .add("plugins.list", |call: Call, _: Empty| async move {
            let plugins = plugins(&call.kernel)?;
            Ok(PluginList {
                plugins: plugins.infos(),
                missing: plugins.missing(),
            })
        })
        .summary("The plugins installed on the server, with their manifests and the addresses of their client halves: what a shell loads.")
        .level(Level::Own);
    methods
        .add("plugins.add", |call: Call, p: PluginAdd| async move {
            let plugins = plugins(&call.kernel)?;
            let asked = rookery_api::PluginSpec {
                spec: p.spec.clone(),
                git: p.git.clone(),
                path: p.path.clone(),
                git_ref: p.git_ref.clone(),
                allow_young: p.allow_young,
            };
            let lines = plugins
                .change(&call.kernel, move |manager| manager.add(&asked))
                .await?;
            call.audit(
                "plugin.add",
                None,
                json!({"spec": p.spec, "git": p.git, "path": p.path}),
            )
            .await;
            report(&plugins, lines)
        })
        .summary("Installs a plugin through Hatchery: a package of the plugin registries, the https address of an egg, or one folder of a git repository.")
        .level(Level::Admin);
    methods
        .add("plugins.update", |call: Call, p: PluginUpdate| async move {
            let plugins = plugins(&call.kernel)?;
            let id = p.id.clone();
            let lines = plugins
                .change(&call.kernel, move |manager| manager.update(id.as_deref()))
                .await?;
            call.audit("plugin.update", None, json!({"plugin": p.id}))
                .await;
            report(&plugins, lines)
        })
        .summary("Updates one plugin, or every plugin, to the newest version its registry offers.")
        .level(Level::Admin);
    methods
        .add("plugins.remove", |call: Call, p: PluginRef| async move {
            let plugins = plugins(&call.kernel)?;
            let id = p.id.clone();
            let lines = plugins
                .change(&call.kernel, move |manager| manager.remove(&id))
                .await?;
            call.audit("plugin.remove", None, json!({"plugin": p.id}))
                .await;
            report(&plugins, lines)
        })
        .summary("Removes a plugin, by its id or package. Its host half stops first.")
        .level(Level::Admin);
    methods
        .add("plugins.check", |call: Call, _: Empty| async move {
            let found = plugins(&call.kernel)?
                .look(|manager| manager.check())
                .await?;
            Ok(PluginUpdates {
                plugins: found
                    .plugins
                    .into_iter()
                    .map(|p| PluginNewer {
                        id: p.id,
                        package: p.package,
                        version: p.version,
                        latest: p.latest,
                        note: p.note,
                    })
                    .collect(),
            })
        })
        .summary("Asks the registries whether a newer version of each plugin is there.")
        .level(Level::Admin);
    methods
        .add("plugins.activate", |call: Call, p: PluginRef| async move {
            let plugins = plugins(&call.kernel)?;
            // One that failed starts again when an admin says so.
            if plugins.info_of(&p.id).is_some_and(|i| i.state == PluginState::Failed) {
                call.require_admin().map_err(|_| {
                    RpcError::forbidden(format!("{} failed: an admin activates it again", p.id))
                })?;
            }
            plugins
                .activate_as(&call.kernel, &p.id, call.is_admin())
                .await?;
            let plugin = plugins
                .get(&p.id)
                .ok_or_else(|| RpcError::not_found(format!("no plugin {}", p.id)))?;
            Ok::<PluginInfo, RpcError>(plugins.info(&plugin))
        })
        .summary("Starts a plugin's host half, as using what it contributes does. A shell asks for it when it shows a view of the plugin.")
        .level(Level::Own);
    methods
        .add("plugins.rpc", |call: Call, p: PluginRpc| async move {
            let plugins = plugins(&call.kernel)?;
            plugins.rpc(&call, &p.plugin, &p.method, p.params).await
        })
        .summary("Calls a method a plugin's host half registered with rookery.rpc.handle, as the caller. The host half says who may.")
        .level(Level::Own);
}
