//! What a plugin's role holds, as a set of privileges, and the difference between what it holds and what it
//! is due. Pure: [`crate::sql::provision`] reads the current set from the catalogs and applies the diff.
//!
//! Only privileges granted to the role itself count. What every role has through `PUBLIC` (Postgres's
//! defaults: `CONNECT` and `TEMPORARY` on the database, `USAGE` on `public` and on the built-in languages
//! and types, `EXECUTE` on functions) cannot be taken from one role and is listed in docs/data.md.

use std::collections::BTreeSet;

use super::names::{Names, View, qualified, quote_ident};

/// An object a privilege is on. Names are already quoted where a statement needs it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Object {
    Database(String),
    Schema(String),
    /// A table, view or materialized view, as `"schema"."name"`.
    Table(String),
    Sequence(String),
    /// As `regprocedure` prints it: `"schema".name(argtypes)`.
    Function(String),
    Column {
        table: String,
        column: String,
    },
    Language(String),
    Type(String),
    Tablespace(String),
    ForeignDataWrapper(String),
    ForeignServer(String),
}

impl Object {
    fn target(&self) -> String {
        match self {
            Object::Database(n) => format!("DATABASE {}", quote_ident(n)),
            Object::Schema(n) => format!("SCHEMA {}", quote_ident(n)),
            Object::Table(n) => format!("TABLE {n}"),
            Object::Sequence(n) => format!("SEQUENCE {n}"),
            Object::Function(n) => format!("FUNCTION {n}"),
            Object::Column { table, .. } => format!("TABLE {table}"),
            Object::Language(n) => format!("LANGUAGE {}", quote_ident(n)),
            Object::Type(n) => format!("TYPE {n}"),
            Object::Tablespace(n) => format!("TABLESPACE {}", quote_ident(n)),
            Object::ForeignDataWrapper(n) => format!("FOREIGN DATA WRAPPER {}", quote_ident(n)),
            Object::ForeignServer(n) => format!("FOREIGN SERVER {}", quote_ident(n)),
        }
    }
}

/// One privilege on one object.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Grant {
    pub on: Object,
    /// `SELECT`, `USAGE`, `CONNECT`, ...
    pub privilege: String,
}

impl Grant {
    fn privilege_list(&self) -> String {
        match &self.on {
            Object::Column { column, .. } => {
                format!("{} ({})", self.privilege, quote_ident(column))
            }
            _ => self.privilege.clone(),
        }
    }
    pub fn grant_sql(&self, role: &str) -> String {
        format!(
            "GRANT {} ON {} TO {}",
            self.privilege_list(),
            self.on.target(),
            quote_ident(role)
        )
    }
    pub fn revoke_sql(&self, role: &str) -> String {
        format!(
            "REVOKE {} ON {} FROM {}",
            self.privilege_list(),
            self.on.target(),
            quote_ident(role)
        )
    }
}

/// What a plugin's role is due: `CONNECT` on the database, `USAGE` on its view schema and `SELECT` on each
/// of its views. Ownership of its raw schema and what is in it is not a grant; it is the plugin's own.
pub fn due(names: &Names, database: &str, views: &[View]) -> BTreeSet<Grant> {
    let mut out = BTreeSet::new();
    let g = |on, p: &str| Grant {
        on,
        privilege: p.to_string(),
    };
    out.insert(g(Object::Database(database.to_string()), "CONNECT"));
    out.insert(g(Object::Schema(names.view_schema.clone()), "USAGE"));
    for v in views {
        out.insert(g(
            Object::Table(qualified(&names.view_schema, &v.name)),
            "SELECT",
        ));
    }
    out
}

/// The statements that bring `current` to `due`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub grant: Vec<Grant>,
    pub revoke: Vec<Grant>,
}

pub fn diff(current: &BTreeSet<Grant>, due: &BTreeSet<Grant>) -> Diff {
    Diff {
        grant: due.difference(current).cloned().collect(),
        revoke: current.difference(due).cloned().collect(),
    }
}
