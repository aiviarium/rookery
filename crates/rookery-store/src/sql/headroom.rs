//! Connection slots (docs/data.md, "Raw SQL / Connection slots").
//!
//! Each plugin role may hold 10 connections. Postgres has `max_connections` slots for the whole server, minus
//! `superuser_reserved_connections` (for superusers) and, since 16, `reserved_connections` (for roles that are
//! members of `pg_use_reserved_connections`). Enough plugins with raw SQL, each holding its 10, take every ordinary
//! slot: then the kernel's own pool, and every other application on the server, cannot connect. A role's limit
//! cannot express a share of the server, so the kernel's protection is a reserve only it may use: a
//! `reserved_connections` at least the size of its pool and `pg_use_reserved_connections` for its database
//! user (`deploy/postgres/init.sh` does both). The kernel reads the numbers at start and says what is short.

use super::Sql;
use super::provision::CONNECTION_LIMIT;
use crate::error::Result;

/// What the server offers and what the kernel's user may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Headroom {
    pub max_connections: i64,
    pub superuser_reserved: i64,
    /// `reserved_connections` (0 before Postgres 16).
    pub reserved: i64,
    pub kernel_is_superuser: bool,
    /// The kernel's user is a member of `pg_use_reserved_connections` (Postgres 16 and later).
    pub kernel_uses_reserve: bool,
    /// Plugin roles that can log in.
    pub plugin_roles: i64,
}

impl Headroom {
    /// The slots ordinary roles share.
    pub fn ordinary(&self) -> i64 {
        (self.max_connections - self.superuser_reserved - self.reserved).max(0)
    }

    /// The slots only the kernel's user may take, whatever the plugins hold.
    pub fn kernel_reserve(&self) -> i64 {
        (if self.kernel_is_superuser {
            self.superuser_reserved
        } else {
            0
        }) + (if self.kernel_is_superuser || self.kernel_uses_reserve {
            self.reserved
        } else {
            0
        })
    }

    /// What is short, for a log: empty when plugin roles cannot starve the kernel's pool of `kernel_pool`.
    pub fn warnings(&self, kernel_pool: i64) -> Vec<String> {
        let mut w = Vec::new();
        if self.plugin_roles == 0 {
            return w;
        }
        let may_hold = self.plugin_roles * i64::from(CONNECTION_LIMIT);
        if self.kernel_reserve() < kernel_pool {
            w.push(format!(
                "plugin roles may hold {may_hold} connections ({} x {CONNECTION_LIMIT}) of the {} that ordinary roles \
                 share, and the kernel's own pool (up to {kernel_pool}) has only {} reserved for it: set \
                 reserved_connections to at least {kernel_pool} and GRANT pg_use_reserved_connections to the kernel's \
                 database user (docs/data.md, \"Raw SQL / Connection slots\")",
                self.plugin_roles,
                self.ordinary(),
                self.kernel_reserve(),
            ));
        } else if may_hold > self.ordinary() {
            w.push(format!(
                "plugin roles may hold {may_hold} connections ({} x {CONNECTION_LIMIT}), more than the {} ordinary \
                 slots: the kernel keeps its reserve, but other applications on this server (the identity provider) \
                 can be starved; raise max_connections",
                self.plugin_roles,
                self.ordinary(),
            ));
        }
        w
    }
}

impl Sql<'_> {
    /// The server's connection slots and the kernel's share of them.
    pub async fn connection_headroom(&self) -> Result<Headroom> {
        let row: (i64, i64, i64, bool, bool, i64) = sqlx::query_as(
            "SELECT current_setting('max_connections')::bigint, \
                    current_setting('superuser_reserved_connections')::bigint, \
                    COALESCE((SELECT setting::bigint FROM pg_settings WHERE name = 'reserved_connections'), 0), \
                    (SELECT rolsuper FROM pg_roles WHERE rolname = current_user), \
                    (EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'pg_use_reserved_connections') \
                       AND pg_has_role(current_user, 'pg_use_reserved_connections', 'MEMBER')), \
                    (SELECT count(*) FROM rookery.plugin_sql s JOIN pg_roles r ON r.rolname = s.role_name WHERE r.rolcanlogin)",
        )
        .fetch_one(self.store.pool())
        .await?;
        Ok(Headroom {
            max_connections: row.0,
            superuser_reserved: row.1,
            reserved: row.2,
            kernel_is_superuser: row.3,
            kernel_uses_reserve: row.4,
            plugin_roles: row.5,
        })
    }
}
