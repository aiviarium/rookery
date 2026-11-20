//! `plugins.*`: what is installed, installing, updating and removing through Hatchery, asking for
//! updates, starting a host half and calling the methods it registered.

use std::sync::Arc;

use rookery_kernel::auth::{AccessChange, AccessEvents};
use rookery_kernel::streams::Initiator;
use rookery_kernel::{Call, Caller, Empty, Kernel, Level, Methods, RpcError};
use rookery_proto::stream::StreamOpened;
use serde_json::json;

use crate::Plugins;
use crate::services;
use crate::wire::{
    PluginAdd, PluginInfo, PluginList, PluginNewer, PluginRef, PluginReport, PluginRpc,
    PluginState, PluginUpdate, PluginUpdates, PluginUpload, RegistryLocation,
};

fn plugins(kernel: &Kernel) -> Result<Arc<Plugins>, RpcError> {
    kernel
        .get_facility::<Plugins>()
        .ok_or_else(|| RpcError::unavailable("this server has no plugin folder"))
}

fn report(
    plugins: &Plugins,
    (mut lines, behind): (Vec<String>, Vec<rookery_api::HostBehind>),
) -> Result<PluginReport, RpcError> {
    // A declaration this version does not serve installs, and whoever changed the plugins is told
    // that it will not do anything.
    for plugin in plugins.all() {
        for warning in &plugin.warnings {
            lines.push(format!("{}: {warning}", plugin.id));
        }
    }
    Ok(PluginReport {
        lines,
        plugins: plugins.infos(),
        behind,
    })
}

/// `plugins.list` for a caller: the server's folder, where a plugin came from and what the folder is
/// missing are for an admin alone.
fn listing(plugins: &Plugins, admin: bool) -> PluginList {
    PluginList {
        plugins: plugins.infos_for(admin),
        missing: if admin { plugins.missing() } else { Vec::new() },
    }
}

/// Adds `plugins.list`, `plugins.add`, `plugins.upload`, `plugins.update`, `plugins.remove`,
/// `plugins.check`, `plugins.reload`, `plugins.registry.list/add/remove`, `plugins.activate` and
/// `plugins.rpc`. When the server leads it reads the plugin folder, records it in
/// `rookery.plugins` and starts the host halves that start with it, if a [`crate::Setup`] was set.
pub fn register(methods: &mut Methods) {
    crate::services::register(methods);
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
            plugins.ready_soon().await?;
            Ok(listing(&plugins, call.is_admin()))
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
            let done = plugins
                .change(&call.kernel, move |manager| manager.add(&asked))
                .await?;
            call.audit(
                "plugin.add",
                None,
                json!({"spec": p.spec, "git": p.git, "path": p.path}),
            )
            .await;
            report(&plugins, done)
        })
        .summary("Installs a plugin through Hatchery: a package of the plugin registries, the https address of an egg, or one folder of a git repository.")
        .level(Level::Admin);
    methods
        .add("plugins.upload", |call: Call, p: PluginUpload| async move {
            let plugins = plugins(&call.kernel)?;
            let egg = rookery_api::unb64(&p.base64)
                .ok_or_else(|| RpcError::invalid("base64: not base64"))?;
            let done = plugins
                .change(&call.kernel, move |manager| manager.upload(&egg))
                .await?;
            call.audit("plugin.upload", None, json!({})).await;
            report(&plugins, done)
        })
        .summary("Installs a plugin from an egg the caller sends, in base64.")
        .level(Level::Admin);
    methods
        .add("plugins.update", |call: Call, p: PluginUpdate| async move {
            let plugins = plugins(&call.kernel)?;
            let id = p.id.clone();
            let done = plugins
                .change(&call.kernel, move |manager| manager.update(id.as_deref()))
                .await?;
            call.audit("plugin.update", None, json!({"plugin": p.id}))
                .await;
            report(&plugins, done)
        })
        .summary("Updates one plugin, or every plugin, to the newest version its registry offers.")
        .level(Level::Admin);
    methods
        .add("plugins.remove", |call: Call, p: PluginRef| async move {
            let plugins = plugins(&call.kernel)?;
            // Its id, which may be named by a package, for the grants it holds.
            let grant_holder = plugins
                .all()
                .iter()
                .find(|plugin| plugin.id == p.id || plugin.package == p.id)
                .map(|plugin| plugin.id.clone())
                .or_else(|| rookery_api::plugin_ids::valid_plugin_id(&p.id).then(|| p.id.clone()));
            let id = p.id.clone();
            let done = plugins
                .change(&call.kernel, move |manager| manager.remove(&id))
                .await?;
            // A grant made before an install does not reach a package that later takes its id.
            if let Some(id) = grant_holder {
                let resources = call
                    .store()
                    .access()
                    .forget_principal(&format!("plugin:{id}"))
                    .await?;
                let events = AccessEvents::of(&call.kernel);
                for resource in resources {
                    events.publish(AccessChange::Resource(resource));
                }
            }
            call.audit("plugin.remove", None, json!({"plugin": p.id}))
                .await;
            report(&plugins, done)
        })
        .summary(
            "Removes a plugin, by its id or package, and its grants. Its host half stops first.",
        )
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
        .add("plugins.reload", |call: Call, _: Empty| async move {
            let plugins = plugins(&call.kernel)?;
            plugins.reload(&call.kernel).await?;
            plugins.reloaded(&call.kernel);
            rookery_hosts::plugins_changed(&call.kernel).await;
            Ok(PluginList {
                plugins: plugins.infos(),
                missing: plugins.missing(),
            })
        })
        .summary("Reads the plugin folder again, as after a change made beside the server.")
        .level(Level::Admin);
    methods
        .add("plugins.registry.list", |call: Call, _: Empty| async move {
            plugins(&call.kernel)?
                .look(|manager| manager.registries())
                .await
        })
        .summary("The plugin registries, with their keys.")
        .level(Level::Admin);
    methods
        .add(
            "plugins.registry.add",
            |call: Call, p: rookery_api::PluginRegistry| async move {
                // A folder of the server is read with its rights, so only a caller on its machine
                // names one.
                if call.caller != Caller::Local
                    && rookery_core::eggs::local_registry(&p.kind, &p.location)
                {
                    return Err(RpcError::forbidden(format!(
                        "{} is a folder of the server's machine, and only a caller there adds it as a registry, over the server's socket: run rookery plugin registry add there",
                        p.location.trim()
                    )));
                }
                let plugins = plugins(&call.kernel)?;
                let audit = json!({"kind": p.kind, "location": p.location});
                let done = plugins
                    .change(&call.kernel, move |manager| manager.add_registry(&p))
                    .await?;
                call.audit("plugin.registry.add", None, audit).await;
                report(&plugins, done)
            },
        )
        .summary("Adds a plugin registry, or changes one, such as to turn it off.")
        .level(Level::Admin);
    methods
        .add(
            "plugins.registry.remove",
            |call: Call, p: RegistryLocation| async move {
                let plugins = plugins(&call.kernel)?;
                let audit = json!({"location": p.location});
                let done = plugins
                    .change(&call.kernel, move |manager| {
                        manager.remove_registry(&p.location)
                    })
                    .await?;
                call.audit("plugin.registry.remove", None, audit).await;
                report(&plugins, done)
            },
        )
        .summary("Removes a plugin registry, by its location or name.")
        .level(Level::Admin);
    methods
        .add("plugins.activate", |call: Call, p: PluginRef| async move {
            let plugins = plugins(&call.kernel)?;
            plugins.ready_soon().await?;
            // One that failed starts again when an admin says so.
            if plugins.info_of(&p.id).is_some_and(|i| i.state == PluginState::Failed) {
                call.require_admin().map_err(|_| {
                    RpcError::forbidden(format!("{} failed: an admin activates it again", p.id))
                })?;
            }
            if plugins.get(&p.id).is_none() {
                return Err(RpcError::not_found(format!("no plugin {}", p.id)));
            }
            let permit = plugins.host.reserve_invocation(&p.id, false)?;
            let activation = {
                let (plugins, kernel, id, admin) = (
                    plugins.clone(), call.kernel.clone(), p.id.clone(), call.is_admin(),
                );
                tokio::spawn(async move {
                    let _permit = permit;
                    plugins.activate_as(&kernel, &id, admin).await
                })
            };
            activation.await.map_err(|_| RpcError::unavailable("the activation ended"))??;
            let plugin = plugins
                .get(&p.id)
                .ok_or_else(|| RpcError::not_found(format!("no plugin {}", p.id)))?;
            Ok::<PluginInfo, RpcError>(plugins.info_for(&plugin, call.is_admin()))
        })
        .summary("Starts a plugin's host half, as using what it contributes does. A shell asks for it when it shows a view of the plugin.")
        .level(Level::Own);
    methods
        .add("plugins.rpc", |call: Call, p: PluginRpc| async move {
            let plugins = plugins(&call.kernel)?;
            plugins.ready_soon().await?;
            plugins.rpc(&call, &p.plugin, &p.method, p.params).await
        })
        .summary("Calls a method a plugin's host half registered with rookery.rpc.handle, as the caller. The host half says who may.")
        .level(Level::Own);
    methods
        .add(
            "plugins.rpc.stream",
            |call: Call, p: PluginRpc| async move {
                // The caller's connection first: a stream needs one, and nothing is started for none.
                call.conn()?;
                let plugins = plugins(&call.kernel)?;
                plugins.ready_soon().await?;
                let source = plugins
                    .rpc_stream(&call, &p.plugin, &p.method, p.params)
                    .await?;
                let stream = call.conn()?.streams().open(Initiator::Peer)?;
                let id = stream.id();
                let (_reader, writer) = stream.split();
                tokio::spawn(services::forward_stream(source, writer));
                Ok(StreamOpened {
                    stream: id,
                    offset: None,
                })
            },
        )
        .summary("Streams a method the plugin's contributes.rpc declares as one, as one JSON value per line.")
        .stream()
        .level(Level::Own)
        .max_timeout(services::max_timeout());
}
