use futures_util::future::BoxFuture;
use rookery_data::plugins::{self, PluginData};
use rookery_kernel::{Kernel, RpcError};
use rookery_plugins::data::{Declared, Transforms};
use rookery_plugins::loaded::Loaded;
use serde_json::Value;

pub(crate) struct DataLayer;

struct HostTransforms(Transforms);

#[async_trait::async_trait]
impl plugins::Transforms for HostTransforms {
    async fn run(&self, plugin: &str, name: &str, migration: &str) -> Result<(), RpcError> {
        if self.0.plugin() != plugin {
            return Err(RpcError::invalid(
                "the transform belongs to a different plugin",
            ));
        }
        self.0.run(name, migration).await
    }
}

impl rookery_plugins::data::PluginData for DataLayer {
    fn apply<'a>(
        &'a self,
        kernel: &'a Kernel,
        plugin: &'a Loaded,
        declared: &'a Declared,
        transforms: Transforms,
    ) -> BoxFuture<'a, Result<(), RpcError>> {
        Box::pin(async move {
            let admin_only = plugin
                .manifest
                .as_ref()
                .and_then(|manifest| manifest.pointer("/contributes/adminPages"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|page| page.get("collections").and_then(Value::as_array))
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            let migrations = declared
                .migrations
                .iter()
                .map(|migration| {
                    serde_json::from_value(migration.body.clone()).map_err(|error| {
                        RpcError::invalid(format!("migrations/{}: {error}", migration.file))
                    })
                })
                .collect::<Result<_, _>>()?;
            let database = declared.database.as_ref().unwrap_or(&Value::Null);
            let data = PluginData {
                plugin: plugin.id.clone(),
                collections: Value::Array(declared.collections.clone()),
                admin_only,
                migrations,
                database: rookery_data::sql::Database {
                    sql: database["sql"] == true,
                    direct: database["direct"] == true,
                    service_migrations: database["migrations"] == "service",
                },
                sql_migrations: rookery_data::sql::read_sql_migrations(&plugin.dir)?,
            };
            plugins::apply(kernel, &data, &HostTransforms(transforms)).await?;
            Ok(())
        })
    }

    fn reconcile<'a>(
        &'a self,
        kernel: &'a Kernel,
        plugins: &'a [Loaded],
    ) -> BoxFuture<'a, Result<(), RpcError>> {
        Box::pin(async move {
            let keep = plugins
                .iter()
                .filter(|plugin| {
                    plugin.valid()
                        && plugin
                            .declared
                            .database
                            .as_ref()
                            .is_some_and(|db| db["sql"] == true)
                })
                .map(|plugin| plugin.id.clone())
                .collect();
            rookery_data::sql::sweep(kernel, keep).await?;
            Ok(())
        })
    }
}
