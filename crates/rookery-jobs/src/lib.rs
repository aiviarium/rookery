//! Jobs, leases, topics, events and notifications.

pub mod events;
pub mod jobs;
pub mod leases;
pub mod notify;
pub mod topics;

use chrono::{DateTime, Utc};
use rookery_kernel::Methods;

pub use jobs::{
    EnqueuePolicy, JobHandler, JobRun, MayEnqueue, Options, add_handler, handles, remove_handler,
    set_enqueue_policy, wake,
};

/// Registers the methods, with the job handlers that run inside the server.
pub fn register(
    methods: &mut Methods,
    handlers: Vec<std::sync::Arc<dyn JobHandler>>,
    options: Options,
) {
    jobs::register(methods, handlers, options);
    leases::register(methods);
    topics::register(methods);
    events::register(methods);
    notify::register(methods);
}

pub(crate) fn time(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A name inside a plugin's own space: `<plugin>/<name>`.
pub(crate) fn scoped(plugin: &str, name: &str) -> Result<String, rookery_kernel::RpcError> {
    if name.is_empty()
        || name.len() > 200
        || name.contains('/')
        || name.chars().any(char::is_control)
    {
        return Err(rookery_kernel::RpcError::invalid(
            "a name is 1 to 200 characters without a slash",
        ));
    }
    Ok(format!("{plugin}/{name}"))
}
