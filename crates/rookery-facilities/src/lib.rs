//! The framework's facilities. Each module has a `register` that adds its methods.

pub mod access;
pub mod admin;
pub mod blobs;
pub mod connections;
pub mod layout;
pub mod secrets;
pub mod settings;
pub mod storage;

use chrono::{DateTime, Utc};
use rookery_kernel::{Call, Methods, RpcError};
use uuid::Uuid;

pub fn register(methods: &mut Methods) {
    access::register(methods);
    admin::register(methods);
    settings::register(methods);
    secrets::register(methods);
    storage::register(methods);
    connections::register(methods);
    blobs::register(methods);
    layout::register(methods);
}

pub(crate) fn time(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub(crate) fn parse_id(what: &str, id: &str) -> Result<Uuid, RpcError> {
    Uuid::parse_str(id).map_err(|_| RpcError::invalid(format!("{id:?} is not the id of a {what}")))
}

/// The user of a call for something of the user scope.
pub(crate) fn own_user(call: &Call) -> Result<Uuid, RpcError> {
    if call.user().is_none() {
        return Err(RpcError::invalid(
            "the user scope is a person's, and this call is from none",
        ));
    }
    call.user_id()
}

/// The space a call names, when the caller holds at least `need` on it.
pub(crate) async fn space_held(
    call: &Call,
    space: Option<&str>,
    need: rookery_kernel::Grant,
) -> Result<Uuid, RpcError> {
    let Some(space) = space else {
        return Err(RpcError::invalid(
            "the space scope names the space in space",
        ));
    };
    let id = parse_id("space", space)?;
    if !crate::access::holds(call, &format!("space://{id}"), need).await? {
        return Err(RpcError::forbidden(format!(
            "this needs {} on space://{id}",
            rookery_kernel::methods::grant_name(need)
        )));
    }
    Ok(id)
}
