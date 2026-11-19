//! `access.*`, `admin.*` and `audit.list`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::facilities::wire;
use crate::methods::Grant;

wire! {
    /// `access.grant`. `principal` is `user:<id>`, `group:<id>`, `plugin:<id>` or `everyone`.
    pub struct AccessGrant {
        pub resource: String,
        pub principal: String,
        pub level: Grant,
    }
}

wire! {
    /// `access.revoke`.
    pub struct AccessRevoke {
        pub resource: String,
        pub principal: String,
    }
}

wire! {
    /// `access.grants` and `access.level`.
    pub struct ResourceRef {
        pub resource: String,
    }
}

wire! {
    pub struct GrantInfo {
        pub principal: String,
        pub level: Grant,
        pub created_by: String,
        pub created_at: String,
    }
}

wire! {
    pub struct GrantList {
        pub resource: String,
        /// The owner's user id, who holds manage.
        pub owner: Option<String>,
        pub grants: Vec<GrantInfo>,
    }
}

wire! {
    /// The caller's level on a resource: none, view, use or manage. An admin holds manage on all.
    pub struct LevelHeld {
        pub level: Option<Grant>,
    }
}

wire! {
    /// `access.levelOf`: the level of another principal.
    pub struct LevelOf {
        pub resource: String,
        pub principal: String,
    }
}

wire! {
    /// `access.require`: `FORBIDDEN` unless the caller holds `level`.
    pub struct AccessRequire {
        pub resource: String,
        pub level: Grant,
    }
}

wire! {
    /// `access.filter`: those of `resources` the caller holds `level` on.
    pub struct AccessFilter {
        pub resources: Vec<String>,
        pub level: Grant,
    }
}

wire! {
    pub struct Resources {
        pub resources: Vec<String>,
    }
}

wire! {
    /// A person, for admins.
    pub struct UserInfo {
        pub id: String,
        pub name: String,
        pub email: Option<String>,
        pub admin: bool,
        pub disabled: bool,
        /// `local` or the identity provider's issuer.
        pub issuer: String,
        pub created_at: String,
        pub last_seen_at: Option<String>,
    }
}

wire! {
    pub struct UserList {
        pub users: Vec<UserInfo>,
    }
}

wire! {
    /// `admin.setRole`.
    pub struct SetRole {
        pub id: String,
        pub admin: bool,
    }
}

wire! {
    /// `admin.setDisabled`.
    pub struct SetDisabled {
        pub id: String,
        pub disabled: bool,
    }
}

wire! {
    pub struct GroupInfo {
        pub id: String,
        pub name: String,
        /// The identity provider's group whose members it has.
        pub claim: Option<String>,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub members: i64,
    }
}

wire! {
    pub struct GroupList {
        pub groups: Vec<GroupInfo>,
    }
}

wire! {
    /// `admin.createGroup`.
    pub struct GroupCreate {
        pub name: String,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub claim: Option<String>,
    }
}

wire! {
    /// `admin.addMember` and `admin.removeMember`.
    pub struct Membership {
        pub group: String,
        pub user: String,
    }
}

wire! {
    /// `audit.list`: the newest first, filtered.
    pub struct AuditQuery {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub resource: Option<String>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub principal: Option<String>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub action: Option<String>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub before: Option<i64>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub limit: Option<i64>,
    }
}

wire! {
    pub struct AuditEntry {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub id: i64,
        pub at: String,
        pub principal: String,
        pub action: String,
        pub resource: Option<String>,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub details: Value,
    }
}

wire! {
    pub struct AuditList {
        pub entries: Vec<AuditEntry>,
    }
}
