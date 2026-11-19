//! Who a read or a write of the data layer is for, and the levels on resources.
//!
//! The kernel builds a [`Caller`] from its call. The store adds the scope filter and checks access from
//! it, and writes record its principal, with the user as on-behalf-of when the two differ.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{Error, Result};

/// The principal every person holds grants as besides their own: `everyone`.
pub const EVERYONE: &str = "everyone";

/// A level on a resource, each including the one before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    View = 1,
    Use = 2,
    Manage = 3,
}

impl Level {
    pub fn as_i16(self) -> i16 {
        self as i16
    }
    pub fn from_i16(v: i16) -> Option<Level> {
        match v {
            1 => Some(Level::View),
            2 => Some(Level::Use),
            3 => Some(Level::Manage),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Level::View => "view",
            Level::Use => "use",
            Level::Manage => "manage",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The caller of a store operation.
///
/// - `user` is the person the call is for, if any.
/// - `principal` is who acts, as grants and the audit log name it: `user:<id>`, `plugin:<id>`,
///   `system:<part>`, `local:socket`.
/// - `space` is the current space, which space-scoped collections need.
/// - `admin` lets the caller do everything.
/// - `admin_of` lets it do everything with one plugin's collections, as a plugin may with its own. It
///   counts for nothing else: a reference into another plugin's collection, or the entity index, reads
///   as the plain caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub user: Option<Uuid>,
    pub principal: String,
    pub space: Option<Uuid>,
    pub admin: bool,
    pub admin_of: Option<String>,
}

impl Caller {
    /// A person acting for themselves.
    pub fn user(id: Uuid) -> Self {
        Caller {
            user: Some(id),
            principal: format!("user:{id}"),
            space: None,
            admin: false,
            admin_of: None,
        }
    }
    /// A plugin, or another principal, acting for a person.
    pub fn on_behalf_of(principal: impl Into<String>, user: Uuid) -> Self {
        Caller {
            user: Some(user),
            principal: principal.into(),
            space: None,
            admin: false,
            admin_of: None,
        }
    }
    /// A plugin acting for itself, with no person, such as its own job.
    pub fn plugin(id: &str) -> Self {
        Caller {
            user: None,
            principal: format!("plugin:{id}"),
            space: None,
            admin: false,
            admin_of: None,
        }
    }
    /// The server itself: an admin, for migrations and maintenance.
    pub fn system() -> Self {
        Caller {
            user: None,
            principal: "system:rookery".into(),
            space: None,
            admin: true,
            admin_of: None,
        }
    }
    pub fn in_space(mut self, space: Uuid) -> Self {
        self.space = Some(space);
        self
    }
    pub fn with_space(mut self, space: Option<Uuid>) -> Self {
        self.space = space;
        self
    }
    pub fn as_admin(mut self) -> Self {
        self.admin = true;
        self
    }
    /// The caller administers the collections of `plugin`, and nothing else.
    pub fn admin_of(mut self, plugin: impl Into<String>) -> Self {
        self.admin_of = Some(plugin.into());
        self
    }
    /// Whether the caller may do everything with the collections of `plugin`.
    pub fn admin_on(&self, plugin: &str) -> bool {
        self.admin || self.admin_of.as_deref() == Some(plugin)
    }
    /// The principal writes record.
    pub fn actor(&self) -> String {
        self.principal.clone()
    }
    /// The user writes record as on-behalf-of: set when the principal acting is not that user.
    pub fn on_behalf_of_user(&self) -> Option<Uuid> {
        match self.user {
            Some(u) if self.principal == format!("user:{u}") => None,
            u => u,
        }
    }
    pub(crate) fn require_user(&self) -> Result<Uuid> {
        self.user.ok_or_else(|| {
            Error::invalid("this is about a person's own things, and the call is for none")
        })
    }
    pub(crate) fn require_space(&self) -> Result<Uuid> {
        self.space
            .ok_or_else(|| Error::invalid("this collection is a space's: name the space"))
    }
}

/// A principal a grant may name: `user:<uuid>`, `group:<uuid>`, `plugin:<id>` or `everyone`.
///
/// A user or group's id is put in the one spelling the callers' principals have, so that
/// `user:<UUID>` in capitals or without hyphens names the same person. A plugin's id is kept as it
/// is: a package's id, and none Rookery keeps for itself.
pub fn principal_name(principal: &str) -> Result<String> {
    if principal == EVERYONE {
        return Ok(principal.to_owned());
    }
    if let Some(id) = principal.strip_prefix("plugin:") {
        return if rookery_api::plugin_ids::valid_plugin_id(id) {
            Ok(format!("plugin:{id}"))
        } else {
            Err(Error::invalid(format!(
                "{principal:?} does not name a plugin after plugin:"
            )))
        };
    }
    let named = ["user:", "group:"].iter().find_map(|kind| {
        let id = principal.strip_prefix(kind)?;
        Uuid::parse_str(id).ok().map(|id| format!("{kind}{id}"))
    });
    named.ok_or_else(|| {
        Error::invalid(format!(
            "{principal:?} is not user:<id>, group:<id>, plugin:<id> or everyone"
        ))
    })
}
