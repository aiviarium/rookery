//! What a caller may do with a host, in the roles the actions of hosts name.

pub use rookery_api::ops::Role;
use rookery_kernel::{Call, Grant};

/// A caller of a method about a host, with the role its level on the host gives.
#[derive(Debug, Clone)]
pub struct Access {
    pub role: Role,
    /// Who it is, as the audit log names them.
    pub subject: Option<String>,
}

impl Access {
    /// The server's own machine, an admin.
    pub fn local() -> Access {
        Access {
            role: Role::Admin,
            subject: None,
        }
    }
}

/// The role a level gives: view and use look, manage operates, and the org role admin is admin.
pub fn role(admin: bool, level: Option<Grant>) -> Option<Role> {
    match (admin, level) {
        (true, _) => Some(Role::Admin),
        (false, Some(Grant::Manage)) => Some(Role::Operate),
        (false, Some(_)) => Some(Role::View),
        (false, None) => None,
    }
}

/// The caller as an access to a host: `None` for the server's own machine, which the handlers treat
/// as local.
pub async fn of(
    call: &Call,
    host: Option<&str>,
) -> Result<Option<Access>, rookery_kernel::RpcError> {
    if matches!(call.caller, rookery_kernel::Caller::Local) {
        return Ok(None);
    }
    let level = match host {
        Some(name) => rookery_facilities::access::level(call, &crate::uri(name)).await?,
        None => None,
    };
    let role = role(call.is_admin(), level).unwrap_or(Role::View);
    Ok(Some(Access {
        role,
        subject: Some(call.audit_name()),
    }))
}

/// What a role is called as a level on a host.
pub fn level_name(role: Role) -> &'static str {
    match role {
        Role::View => "view",
        Role::Operate => "manage",
        Role::Admin => "the role admin",
    }
}
